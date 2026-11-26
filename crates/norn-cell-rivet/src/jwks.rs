//! The issuer's key document (JWKS), fetched from its pinned location and
//! shared by every cell actor and the router's gate in this process, so a
//! runner fetches it at most once a minute however many cells open.

use std::sync::atomic::{AtomicU64, Ordering};
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

/// Outcome of the most recent completed fetch attempt, shared with concurrent
/// waiters whose ticket predates `seq` so an issuer outage does not serialize
/// N redundant timeouts behind `JWKS_FETCH_LOCK`.
#[derive(Clone)]
struct LastAttempt {
    url: String,
    seq: u64,
    outcome: std::result::Result<Jwks, String>,
}

static LAST_JWKS: std::sync::Mutex<Option<Jwks>> = std::sync::Mutex::new(None);
static LAST_ATTEMPT: std::sync::Mutex<Option<LastAttempt>> = std::sync::Mutex::new(None);
/// Monotonic fetch generation incremented under `JWKS_FETCH_LOCK` before each
/// outbound HTTP request so coalescing never depends on wall-clock time.
static JWKS_FETCH_SEQ: AtomicU64 = AtomicU64::new(0);
/// Coalesces concurrent JWKS fetches within one runner process.
static JWKS_FETCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn last_jwks() -> Result<Option<Jwks>> {
    Ok(LAST_JWKS
        .lock()
        .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))?
        .clone())
}

fn last_attempt() -> Result<Option<LastAttempt>> {
    Ok(LAST_ATTEMPT
        .lock()
        .map_err(|_poisoned| anyhow!("the JWKS attempt cache is poisoned"))?
        .clone())
}

fn cached_jwks(url: &str) -> Result<Option<Jwks>> {
    Ok(last_jwks()?.filter(|entry| {
        entry.url == url
            && now_ms()
                .checked_sub(entry.at)
                .is_some_and(|age| age < OPEN_RETRY_MS)
    }))
}

fn cached_attempt_after(url: &str, ticket: u64) -> Result<Option<Result<Jwks>>> {
    Ok(last_attempt()?
        .filter(|entry| entry.url == url && entry.seq > ticket)
        .map(|entry| entry.outcome.map_err(anyhow::Error::msg)))
}

async fn fetch_jwks_once(http: &reqwest::Client, url: &str) -> Result<Jwks> {
    let response = http
        .get(url)
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
    Ok(Jwks {
        url: url.to_owned(),
        text,
        at: now_ms(),
    })
}

/// The pinned JWKS document at `url` and when it was fetched; `reuse` accepts
/// this process's last one while it is younger than `OPEN_RETRY_MS`. The
/// actors and the router's gate share it, so a runner fetches the issuer's
/// keys at most once a minute for both. Concurrent `reuse = false` refreshes
/// coalesce onto the outcome (success or error) of a fetch that started after
/// the caller began waiting (tracked by a monotonic generation counter, never
/// wall-clock time).
pub(crate) async fn fetch_jwks(http: &reqwest::Client, url: String, reuse: bool) -> Result<Jwks> {
    if reuse && let Some(last) = cached_jwks(&url)? {
        return Ok(last);
    }
    let ticket = JWKS_FETCH_SEQ.load(Ordering::Acquire);
    let _singleflight = JWKS_FETCH_LOCK.lock().await;
    if reuse && let Some(last) = cached_jwks(&url)? {
        return Ok(last);
    }
    if let Some(outcome) = cached_attempt_after(&url, ticket)? {
        return outcome;
    }
    let seq = JWKS_FETCH_SEQ
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);
    let outcome = fetch_jwks_once(http, &url).await;
    let recorded = match &outcome {
        Ok(fetched) => {
            *LAST_JWKS
                .lock()
                .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))? =
                Some(fetched.clone());
            Ok(fetched.clone())
        }
        Err(error) => Err(format!("{error:#}")),
    };
    *LAST_ATTEMPT
        .lock()
        .map_err(|_poisoned| anyhow!("the JWKS attempt cache is poisoned"))? = Some(LastAttempt {
        url,
        seq,
        outcome: recorded,
    });
    outcome
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Waker};

    use axum::response::IntoResponse as _;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer as _, SigningKey};
    use http::StatusCode;
    use norn_api::scope::CellScope;
    use norn_cell_do::principal::IssuerKeys;
    use serde_json::json;
    use tokio::sync::Notify;

    use super::*;
    use crate::egress::outbound_client;

    const ISSUER: &str = "https://studio.test";
    static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn now_secs() -> u64 {
        now_ms() / 1000
    }

    fn jwks_doc(signer: &SigningKey, kid: &str) -> String {
        json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "alg": "EdDSA",
                "kid": kid,
                "x": URL_SAFE_NO_PAD.encode(signer.verifying_key().as_bytes())
            }]
        })
        .to_string()
    }

    fn signed_token(signer: &SigningKey, kid: &str, org: &str) -> String {
        let issued = now_secs().saturating_sub(10);
        let claims = json!({
            "iss": ISSUER,
            "aud": "norn-host",
            "sub": "u1",
            "iat": issued,
            "exp": issued.saturating_add(290),
            "norn": { "kind": "user", "org": org, "role": "owner", "amr": "password" }
        });
        let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": kid }).to_string());
        let body = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signature = signer.sign(format!("{head}.{body}").as_bytes());
        format!(
            "{head}.{body}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    #[tokio::test]
    async fn fetch_jwks_recovers_after_backward_clock_step() -> Result<()> {
        let _serial = TEST_LOCK.lock().await;
        let signer = SigningKey::from_bytes(&[7; 32]);
        let document = jwks_doc(&signer, "k1");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let fetches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&fetches);
        let body = document.clone();
        let app = axum::Router::new().fallback(move || {
            let counted = Arc::clone(&counted);
            let body = body.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                (StatusCode::OK, body).into_response()
            }
        });
        let server = tokio::spawn(async move {
            let _result = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}/jwks");
        let future_ms = now_ms().saturating_add(600_000);
        {
            let _seed_lock = JWKS_FETCH_LOCK.lock().await;
            let future_seq = JWKS_FETCH_SEQ
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1);
            let seeded = Jwks {
                url: url.clone(),
                text: document.clone(),
                at: future_ms,
            };
            *LAST_JWKS
                .lock()
                .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))? = Some(seeded.clone());
            *LAST_ATTEMPT
                .lock()
                .map_err(|_poisoned| anyhow!("the JWKS attempt cache is poisoned"))? =
                Some(LastAttempt {
                    url: url.clone(),
                    seq: future_seq,
                    outcome: Ok(seeded),
                });
        }

        let keys = IssuerKeys::new(ISSUER, &document, future_ms / 1000)
            .map_err(|refused| anyhow!(refused.message))?;
        let token = signed_token(&signer, "k1", "org-1");
        let scope = CellScope {
            org_id: "org-1".to_owned(),
            environment_id: "prod".to_owned(),
        };
        assert!(keys.principal_with(&token, &scope, now_secs).is_none());
        assert!(keys.refresh_due_with(now_secs));

        let http = outbound_client()?;
        let refreshed = fetch_jwks(&http, url, false).await?;
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert!(refreshed.at < future_ms);

        keys.replace_jwks_at(&refreshed.text, refreshed.at / 1000, now_secs)
            .map_err(|refused| anyhow!(refused.message))?;
        assert!(keys.principal_with(&token, &scope, now_secs).is_some());
        assert!(keys.principal_with(&token, &scope, now_secs).is_some());

        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn fetch_jwks_does_not_reuse_pre_arrival_inflight_fetch() -> Result<()> {
        let _serial = TEST_LOCK.lock().await;
        let old_doc = jwks_doc(&SigningKey::from_bytes(&[1; 32]), "old");
        let new_doc = jwks_doc(&SigningKey::from_bytes(&[2; 32]), "new");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let fetches = Arc::new(AtomicUsize::new(0));
        let first_started = Arc::new(Notify::new());
        let release_first = Arc::new(Notify::new());

        let counted = Arc::clone(&fetches);
        let started = Arc::clone(&first_started);
        let release = Arc::clone(&release_first);
        let first_body = old_doc.clone();
        let second_body = new_doc.clone();
        let app = axum::Router::new().fallback(move || {
            let counted = Arc::clone(&counted);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            let first_body = first_body.clone();
            let second_body = second_body.clone();
            async move {
                let n = counted.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    started.notify_one();
                    release.notified().await;
                    (StatusCode::OK, first_body).into_response()
                } else {
                    (StatusCode::OK, second_body).into_response()
                }
            }
        });
        let server = tokio::spawn(async move {
            let _result = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}/jwks");
        let http = outbound_client()?;
        let first_http = http.clone();
        let first_url = url.clone();
        let first = tokio::spawn(async move { fetch_jwks(&first_http, first_url, false).await });
        first_started.notified().await;

        let mut second = std::pin::pin!(fetch_jwks(&http, url, false));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(second.as_mut().poll(&mut cx).is_pending());
        release_first.notify_one();

        let (first_joined, second_res) = tokio::join!(first, second);
        let first_res = first_joined??;
        let second_res = second_res?;
        assert_eq!(first_res.text, old_doc);
        assert_eq!(second_res.text, new_doc);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);

        server.abort();
        Ok(())
    }

    #[tokio::test]
    async fn fetch_jwks_coalesces_concurrent_failed_refreshes() -> Result<()> {
        let _serial = TEST_LOCK.lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let fetches = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&fetches);
        let app = axum::Router::new().fallback(move || {
            let counted = Arc::clone(&counted);
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                (StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response()
            }
        });
        let server = tokio::spawn(async move {
            let _result = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}/jwks");
        let http = outbound_client()?;
        let hold = JWKS_FETCH_LOCK.lock().await;
        let mut f1 = std::pin::pin!(fetch_jwks(&http, url.clone(), false));
        let mut f2 = std::pin::pin!(fetch_jwks(&http, url.clone(), false));
        let mut f3 = std::pin::pin!(fetch_jwks(&http, url.clone(), false));
        let mut f4 = std::pin::pin!(fetch_jwks(&http, url.clone(), false));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(f1.as_mut().poll(&mut cx).is_pending());
        assert!(f2.as_mut().poll(&mut cx).is_pending());
        assert!(f3.as_mut().poll(&mut cx).is_pending());
        assert!(f4.as_mut().poll(&mut cx).is_pending());
        drop(hold);

        let (r1, r2, r3, r4) = tokio::join!(f1, f2, f3, f4);
        assert!(r1.is_err() && r2.is_err() && r3.is_err() && r4.is_err());
        assert_eq!(fetches.load(Ordering::SeqCst), 1);

        // A later sequential arrival still retries the issuer.
        assert!(fetch_jwks(&http, url, false).await.is_err());
        assert_eq!(fetches.load(Ordering::SeqCst), 2);

        server.abort();
        Ok(())
    }
}
