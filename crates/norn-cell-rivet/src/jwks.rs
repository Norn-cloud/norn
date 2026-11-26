//! The issuer's key document (JWKS), fetched from its pinned location and
//! shared by every cell actor and the router's gate in this process, so a
//! runner fetches it at most once a minute however many cells open.

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};

use crate::actor::{OPEN_RETRY_MS, now_ms};
use crate::egress::read_bounded;

/// A hung JWKS fetch must not hold the cell.
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest JWKS document read from the issuer, in bytes.
const MAX_JWKS_BYTES: usize = 256 * 1024;

/// The JWKS document this process fetched last, reused by cold opens for
/// `OPEN_RETRY_MS` (one issuer fetch a minute however many cells open).
#[derive(Clone)]
pub(crate) struct Jwks {
    url: String,
    /// The document.
    pub(crate) text: String,
    /// When it was fetched (Unix ms).
    pub(crate) at: u64,
}

static LAST_JWKS: std::sync::Mutex<Option<Jwks>> = std::sync::Mutex::new(None);
/// Coalesces concurrent JWKS fetches within one runner process.
static JWKS_FETCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn cached_jwks(url: &str) -> Result<Option<Jwks>> {
    let last = LAST_JWKS
        .lock()
        .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))?
        .clone();
    Ok(last.filter(|entry| {
        entry.url == url
            && now_ms()
                .checked_sub(entry.at)
                .is_some_and(|age| age < OPEN_RETRY_MS)
    }))
}

fn cached_jwks_since(url: &str, min_at: u64) -> Result<Option<Jwks>> {
    let last = LAST_JWKS
        .lock()
        .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))?
        .clone();
    Ok(last.filter(|entry| entry.url == url && entry.at >= min_at))
}

/// The pinned JWKS document at `url` and when it was fetched; `reuse` accepts
/// this process's last one while it is younger than `OPEN_RETRY_MS`. The
/// actors and the router's gate share it, so a runner fetches the issuer's
/// keys at most once a minute for both. Concurrent `reuse = false` refreshes
/// coalesce on any fetch completed at or after the caller started waiting.
pub(crate) async fn fetch_jwks(http: &reqwest::Client, url: String, reuse: bool) -> Result<Jwks> {
    if reuse && let Some(last) = cached_jwks(&url)? {
        return Ok(last);
    }
    let started = now_ms();
    let _singleflight = JWKS_FETCH_LOCK.lock().await;
    if reuse && let Some(last) = cached_jwks(&url)? {
        return Ok(last);
    }
    if let Some(fresh) = cached_jwks_since(&url, started)? {
        return Ok(fresh);
    }
    let response = http
        .get(&url)
        .timeout(JWKS_TIMEOUT)
        .send()
        .await
        .context("JWKS fetch failed")?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(anyhow!("JWKS fetch failed: HTTP {}", response.status()));
    }
    let bytes = read_bounded(response, MAX_JWKS_BYTES)
        .await
        .ok_or_else(|| anyhow!("JWKS read failed or exceeded {MAX_JWKS_BYTES} bytes"))?;
    let text = String::from_utf8(bytes).context("JWKS is not UTF-8")?;
    let fetched = Jwks {
        url,
        text,
        at: now_ms(),
    };
    *LAST_JWKS
        .lock()
        .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))? = Some(fetched.clone());
    Ok(fetched)
}
