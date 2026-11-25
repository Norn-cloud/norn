//! The issuer's key document (JWKS), fetched from its pinned location and
//! shared by every cell actor and the router's gate in this process, so a
//! runner fetches it at most once a minute however many cells open.

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};

use crate::actor::{OPEN_RETRY_MS, now_ms};

/// A hung JWKS fetch must not hold the cell.
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);

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

/// The pinned JWKS document at `url` and when it was fetched; `reuse` accepts
/// this process's last one while it is younger than `OPEN_RETRY_MS`. The
/// actors and the router's gate share it, so a runner fetches the issuer's
/// keys at most once a minute for both.
pub(crate) async fn fetch_jwks(http: &reqwest::Client, url: String, reuse: bool) -> Result<Jwks> {
    if reuse {
        let last = LAST_JWKS
            .lock()
            .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))?
            .clone();
        if let Some(last) = last
            && last.url == url
            && now_ms()
                .checked_sub(last.at)
                .is_some_and(|age| age < OPEN_RETRY_MS)
        {
            return Ok(last);
        }
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
    let text = response.text().await.context("JWKS read failed")?;
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
