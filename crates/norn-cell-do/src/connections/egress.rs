//! The one outbound HTTP client for connector calls (token exchange,
//! refresh, revocation, provider API calls).
//!
//! EGRESS-URL-1: a URL is https with no credentials in it, to a host that is
//! not internal by the shared rules ([`norn_api::net`], NET-ADDR-1); plain
//! http or an internal host only when the operator lists the host
//! (`NORN_SOURCE_ALLOWED_HOSTS`, the same list sources use).
//! EGRESS-PIN-1: a name is resolved once per connection, within
//! [`Timeouts::resolve`], every address it resolves to is checked after
//! resolution (any internal address refuses the whole answer unless the host
//! is listed), and the connection is made only to the addresses checked, so a
//! second answer (DNS rebinding) is never used.
//! EGRESS-REDIRECT-1: [`Egress::send`] follows redirects here, at most
//! [`MAX_REDIRECTS`], each hop's URL checked as the first. A hop that changes
//! origin keeps only [`CROSS_ORIGIN_SAFE_HEADERS`] (deny by default, the list
//! of `OpenConnector` v1.9.4 `src/core/guarded-fetch.ts`, Apache-2.0); 303, and
//! 301/302 after a POST, become a body-less GET. A hop is refused when it
//! would resend a body to another origin (307/308) or go from https to plain
//! http, whatever the allowlist says. [`Egress::send_direct`] follows none:
//! OAuth token exchange, refresh and revocation carry client secrets, codes,
//! verifiers and tokens in their bodies and use it.
//! EGRESS-PROXY-1: no proxy. `NORN_EGRESS_PROXY` is refused at startup
//! ([`refuse_proxy`]) until a CONNECT to the checked address, keeping the TLS
//! name and `Host`, exists; ambient `HTTP(S)_PROXY` variables are ignored.
//! EGRESS-RETRY-1: idempotent methods are retried on a transport failure or a
//! 429/502/503/504 answer, at most [`MAX_ATTEMPTS`] times with exponential
//! backoff (or `Retry-After`, capped); a POST is sent once.
//! EGRESS-DEADLINE-1: one call, with every redirect, retry, backoff and
//! lookup, ends within [`Timeouts::total`].
//! EGRESS-QUIET-1: errors carry fixed codes, never a peer's text, and the
//! `log` facade is capped at `info` for this crate's builds (`Cargo.toml`),
//! so ureq's debug and trace records (request lines, wire rows) are compiled
//! out.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::header::{HeaderName, LOCATION, RETRY_AFTER};
use http::{HeaderMap, Method, Request, Response};
use ureq::config::Config;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// The server variable that would name an egress proxy; refused
/// (EGRESS-PROXY-1).
pub const PROXY_VAR: &str = "NORN_EGRESS_PROXY";
/// Most redirect hops followed.
pub const MAX_REDIRECTS: usize = 10;
/// Most attempts of an idempotent request.
pub const MAX_ATTEMPTS: u32 = 3;
/// Largest response body read, in bytes.
pub const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;

/// Headers a redirect that changes origin keeps; every other header (any
/// credential, whatever its name) is dropped. From `OpenConnector` v1.9.4
/// (`crossOriginSafeHeaders`, sha d89dc9f2, Apache-2.0; its NOTICE is
/// `registry/connections/NOTICE`).
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

/// Refuse a configured egress proxy (EGRESS-PROXY-1): proxy mode would let
/// the proxy resolve the name again, losing EGRESS-PIN-1.
///
/// # Errors
/// [`PROXY_VAR`] is set to anything but blank.
pub fn refuse_proxy(value: Option<&str>) -> Result<(), EgressError> {
    if value.is_some_and(|value| !value.trim().is_empty()) {
        return Err(EgressError::Config(format!(
            "{PROXY_VAR} is not supported yet: a proxy would resolve connector hosts again, \
             outside the egress address checks; unset it"
        )));
    }
    Ok(())
}

/// Why an outbound call did not complete. Never carries a header value, a
/// body or a peer's error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressError {
    /// The URL or a resolved address is refused (EGRESS-URL-1, EGRESS-PIN-1).
    Forbidden(String),
    /// The name did not resolve, or not in time.
    Unresolved(String),
    /// Too many redirects, one without a usable `Location`, or one refused
    /// (EGRESS-REDIRECT-1).
    Redirect(&'static str),
    /// The connection or exchange failed; a fixed kind.
    Transport(&'static str),
    /// The call's whole deadline passed (EGRESS-DEADLINE-1).
    Deadline,
    /// The configuration is malformed or refused.
    Config(String),
}

impl fmt::Display for EgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forbidden(why) => write!(f, "egress refused: {why}"),
            Self::Unresolved(host) => write!(f, "{host} did not resolve"),
            Self::Redirect(why) => write!(f, "redirect refused: {why}"),
            Self::Transport(kind) => write!(f, "request failed: {kind}"),
            Self::Deadline => f.write_str("the call took too long"),
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

/// The client's time bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Connecting to one address.
    pub connect: Duration,
    /// Resolving one name (EGRESS-PIN-1).
    pub resolve: Duration,
    /// One whole call: every redirect, retry, backoff and lookup
    /// (EGRESS-DEADLINE-1).
    pub total: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            resolve: Duration::from_secs(5),
            total: Duration::from_secs(60),
        }
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

/// EGRESS-PIN-1: resolves once, within the resolve bound, and returns only
/// checked addresses, which ureq then connects to.
#[derive(Debug)]
struct PinnedResolver {
    lookup: Arc<dyn Lookup>,
    policy: Arc<Policy>,
    resolve: Duration,
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
    /// `lookup` on its own thread, waited for at most `bound`: a resolver
    /// that hangs costs the caller `bound`, never more.
    fn bounded_lookup(&self, host: &str, port: u16, bound: Duration) -> Option<Vec<IpAddr>> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let lookup = Arc::clone(&self.lookup);
        let name = host.to_owned();
        std::thread::Builder::new()
            .name("norn-egress-dns".to_owned())
            .spawn(move || {
                // The receiver may have given up; nothing to tell it then.
                let _late = send.send(lookup.lookup(&name, port));
            })
            .ok()?;
        receive.recv_timeout(bound).ok()?.ok()
    }

    fn checked(&self, host: &str, port: u16, bound: Duration) -> Result<Vec<IpAddr>, EgressError> {
        let addresses = match host.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => self
                .bounded_lookup(host, port, bound.min(self.resolve))
                .ok_or_else(|| EgressError::Unresolved(host.to_owned()))?,
        };
        if addresses.is_empty() {
            return Err(EgressError::Unresolved(host.to_owned()));
        }
        let listed = self
            .policy
            .listed(&url::Host::parse(host).map_or_else(|_| host.to_owned(), |h| h.to_string()));
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
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let (host, port) = host_port(uri)
            .ok_or_else(|| refused(EgressError::Forbidden("a URL has no host".to_owned())))?;
        let addresses = self.checked(&host, port, *timeout.after).map_err(refused)?;
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
    total: Duration,
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

/// Whether a redirect is followed (EGRESS-REDIRECT-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    Redirects,
    None,
}

/// What a redirect hop does to the next request (EGRESS-REDIRECT-1); a pure
/// decision, checked before anything is sent to `next`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hop {
    /// The next request is a body-less GET (or stays HEAD).
    pub to_get: bool,
    /// The next request keeps only [`CROSS_ORIGIN_SAFE_HEADERS`].
    pub cross_origin: bool,
}

/// Decide a redirect from `from` to `next` answered with `status` to a
/// `method` request that carries a body (`has_body`).
///
/// # Errors
/// [`EgressError::Redirect`]: https to plain http, or a 307/308 that would
/// resend a body to another origin.
pub fn redirect_hop(
    from: &url::Url,
    next: &url::Url,
    status: u16,
    method: &Method,
    has_body: bool,
) -> Result<Hop, EgressError> {
    if from.scheme() == "https" && next.scheme() != "https" {
        return Err(EgressError::Redirect("a redirect leaves https"));
    }
    let to_get = status == 303 || (matches!(status, 301 | 302) && *method == Method::POST);
    let cross_origin = next.origin() != from.origin();
    if cross_origin && has_body && !to_get {
        return Err(EgressError::Redirect(
            "a redirect would resend a body to another origin",
        ));
    }
    Ok(Hop {
        to_get,
        cross_origin,
    })
}

impl Egress {
    /// A client checking with `allowed_hosts` (the parsed
    /// `NORN_SOURCE_ALLOWED_HOSTS`), resolving with `lookup`, with the
    /// default [`Timeouts`].
    #[must_use]
    pub fn new(allowed_hosts: Vec<String>, lookup: Arc<dyn Lookup>) -> Self {
        Self::with_timeouts(allowed_hosts, lookup, Timeouts::default())
    }

    /// [`Self::new`] with other time bounds.
    #[must_use]
    pub fn with_timeouts(
        allowed_hosts: Vec<String>,
        lookup: Arc<dyn Lookup>,
        timeouts: Timeouts,
    ) -> Self {
        let policy = Arc::new(Policy { allowed_hosts });
        let config = Config::builder()
            // No proxy, ambient or configured (EGRESS-PROXY-1).
            .proxy(None)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_connect(Some(timeouts.connect))
            .timeout_global(Some(timeouts.total))
            .user_agent("norn-connect/1")
            .build();
        let resolver = Arc::new(PinnedResolver {
            lookup,
            policy,
            resolve: timeouts.resolve,
        });
        let agent = ureq::Agent::with_parts(
            config,
            DefaultConnector::default(),
            Shared(Arc::clone(&resolver)),
        );
        Self {
            agent,
            resolver,
            total: timeouts.total,
            sleep: std_sleep,
        }
    }

    /// The same client with `sleep` between retries (tests pass a no-op).
    #[must_use]
    pub fn with_sleep(mut self, sleep: fn(Duration)) -> Self {
        self.sleep = sleep;
        self
    }

    /// Send `request`, following redirects (EGRESS-REDIRECT-1) and retrying
    /// idempotent methods (EGRESS-RETRY-1), within the total deadline
    /// (EGRESS-DEADLINE-1).
    ///
    /// # Errors
    /// [`EgressError`].
    pub fn send(&self, request: Request<Vec<u8>>) -> Result<EgressResponse, EgressError> {
        self.run(request, Follow::Redirects)
    }

    /// Send `request` once per attempt and follow no redirect: a 3xx answer
    /// is refused, nothing is sent to its `Location` (EGRESS-REDIRECT-1).
    /// For OAuth token exchange, refresh and revocation.
    ///
    /// # Errors
    /// [`EgressError`]; [`EgressError::Redirect`] for any 3xx.
    pub fn send_direct(&self, request: Request<Vec<u8>>) -> Result<EgressResponse, EgressError> {
        self.run(request, Follow::None)
    }

    fn run(
        &self,
        request: Request<Vec<u8>>,
        follow: Follow,
    ) -> Result<EgressResponse, EgressError> {
        let now = Instant::now();
        let deadline = now.checked_add(self.total).unwrap_or(now);
        let (parts, body) = request.into_parts();
        let mut method = parts.method;
        let mut headers = parts.headers;
        let mut body = Some(body).filter(|body| !body.is_empty());
        let mut url = url::Url::parse(&parts.uri.to_string())
            .map_err(|_error| EgressError::Forbidden("not an absolute URL".to_owned()))?;
        for hop in 0..=MAX_REDIRECTS {
            self.resolver.policy.check_url(&url)?;
            let response = self.attempt(&method, &url, &headers, body.as_deref(), deadline)?;
            let status = response.status().as_u16();
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if follow == Follow::None {
                return Err(EgressError::Redirect("this call follows no redirect"));
            }
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
                return Err(EgressError::Redirect("too many redirects"));
            }
            let next = url
                .join(location)
                .map_err(|_error| EgressError::Redirect("a Location is not a URL"))?;
            let decided = redirect_hop(&url, &next, status, &method, body.is_some())?;
            if decided.to_get {
                if method != Method::HEAD {
                    method = Method::GET;
                }
                body = None;
                for name in BODY_HEADERS {
                    headers.remove(name);
                }
            }
            if decided.cross_origin {
                strip_cross_origin(&mut headers);
            }
            url = next;
        }
        Err(EgressError::Redirect("too many redirects"))
    }

    fn attempt(
        &self,
        method: &Method,
        url: &url::Url,
        headers: &HeaderMap,
        body: Option<&[u8]>,
        deadline: Instant,
    ) -> Result<EgressResponse, EgressError> {
        let idempotent = matches!(
            *method,
            Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
        );
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(EgressError::Deadline);
            }
            let outcome = self.once(method, url, headers, body, left);
            let wait = if let Ok(response) = &outcome
                && matches!(response.status().as_u16(), 429 | 502 | 503 | 504)
            {
                backoff(attempt, response.headers())
            } else if matches!(outcome, Err(EgressError::Transport(_))) {
                backoff(attempt, &HeaderMap::new())
            } else {
                return outcome;
            };
            // Retry only an idempotent call, and only if the wait still
            // leaves time before the deadline.
            let left = deadline.saturating_duration_since(Instant::now());
            if !idempotent || attempt >= MAX_ATTEMPTS || wait >= left {
                return outcome;
            }
            (self.sleep)(wait);
        }
    }

    fn once(
        &self,
        method: &Method,
        url: &url::Url,
        headers: &HeaderMap,
        body: Option<&[u8]>,
        left: Duration,
    ) -> Result<EgressResponse, EgressError> {
        let mut request = Request::builder().method(method.clone()).uri(url.as_str());
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let request = request
            .body(body.map(<[u8]>::to_vec).unwrap_or_default())
            .map_err(|_error| EgressError::Transport("malformed_request"))?;
        // This attempt may use what is left of the call's deadline.
        let request = self
            .agent
            .configure_request(request)
            .timeout_global(Some(left))
            .build();
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

/// A ureq error as a fixed kind (EGRESS-QUIET-1): no peer text survives.
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
    } else if matches!(
        error,
        ureq::Error::Timeout(ureq::Timeout::Global | ureq::Timeout::PerCall)
    ) {
        EgressError::Deadline
    } else if matches!(error, ureq::Error::Timeout(_)) {
        // One step (connect, a read) timed out; the call's deadline has not.
        EgressError::Transport("timeout")
    } else if matches!(error, ureq::Error::BodyExceedsLimit(_)) {
        EgressError::Transport("body_too_large")
    } else if matches!(error, ureq::Error::Tls(_)) {
        EgressError::Transport("tls")
    } else if matches!(error, ureq::Error::ConnectionFailed) {
        EgressError::Transport("connect")
    } else if matches!(error, ureq::Error::Io(_)) {
        EgressError::Transport("io")
    } else {
        EgressError::Transport("protocol")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> url::Url {
        url::Url::parse(text).expect("a test URL")
    }

    /// EGRESS-REDIRECT-1: never from https to http, even to a listed host.
    #[test]
    fn a_redirect_never_leaves_https() {
        for status in [301, 302, 303, 307, 308] {
            assert_eq!(
                redirect_hop(
                    &url("https://api.example.com/a"),
                    &url("http://api.example.com/b"),
                    status,
                    &Method::GET,
                    false,
                ),
                Err(EgressError::Redirect("a redirect leaves https")),
                "{status}"
            );
        }
        assert!(
            redirect_hop(
                &url("http://127.0.0.1:1/a"),
                &url("https://api.example.com/b"),
                302,
                &Method::GET,
                false
            )
            .is_ok()
        );
    }

    /// EGRESS-REDIRECT-1: a 307/308 keeps its body, so it never crosses origins.
    #[test]
    fn a_body_never_crosses_origins() {
        let from = url("https://token.example.com/token");
        let away = url("https://evil.example.net/steal");
        for status in [307, 308] {
            assert_eq!(
                redirect_hop(&from, &away, status, &Method::POST, true),
                Err(EgressError::Redirect(
                    "a redirect would resend a body to another origin"
                ))
            );
            assert_eq!(
                redirect_hop(
                    &from,
                    &url("https://token.example.com/v2"),
                    status,
                    &Method::POST,
                    true
                ),
                Ok(Hop {
                    to_get: false,
                    cross_origin: false
                })
            );
        }
        // 303, and 302 after a POST, drop the body: allowed, credentials stripped.
        for status in [302, 303] {
            assert_eq!(
                redirect_hop(&from, &away, status, &Method::POST, true),
                Ok(Hop {
                    to_get: true,
                    cross_origin: true
                })
            );
        }
    }

    #[test]
    fn a_proxy_is_refused() {
        assert!(refuse_proxy(None).is_ok());
        assert!(refuse_proxy(Some("  ")).is_ok());
        let refused = refuse_proxy(Some("http://proxy.internal:3128"));
        assert!(
            matches!(&refused, Err(EgressError::Config(why)) if why.contains(PROXY_VAR) && why.contains("not supported")),
            "{refused:?}"
        );
    }
}
