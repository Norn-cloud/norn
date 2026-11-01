//! Shopify webhooks to raw observations (T12-DP-4).
//!
//! Shopify signs the raw body with the app's client secret:
//! `X-Shopify-Hmac-Sha256` is base64 HMAC-SHA256. Identity comes from the
//! headers and the resource body:
//!
//! - `source_id` is `{resource}/{id}`, where `resource` is the topic prefix
//!   (`orders/updated` gives `orders`) and `id` is the body's `id`.
//! - `event_id` is `X-Shopify-Event-Id`, falling back to
//!   `X-Shopify-Webhook-Id`. Shopify redelivers with the same ids, so
//!   duplicates converge.
//! - `kind` is `delete` for `*/delete` topics, otherwise `upsert`.
//! - `source_version` is the body's `updated_at` (upserts) or
//!   `X-Shopify-Triggered-At` (deletes and bodies without `updated_at`),
//!   normalized to UTC microseconds ([`normalize_instant`]).
//!
//! DP-SHOPIFY-1: authenticate raw bytes before reading headers or payload.
//! DP-SHOPIFY-2: configured shop and required identity fields refuse mismatches.
//! DP-SHOPIFY-3: signed replay preserves exact IDs, normalized source time and
//! pipeline provenance through submission and convergence (`shopify_ingest_conforms`).
//! Observations are history, not authorization.

use serde_json::Value;

use crate::raw_observation::{
    ObservationDraft, ObservationKind, RawObservation, RedactionPolicy, parse_exact_json,
};
use crate::webhook::{IngestError, normalize_instant, verify_hmac_sha256};

/// `X-Shopify-Hmac-Sha256`: the body signature.
pub const SHOPIFY_HMAC_HEADER: &str = "X-Shopify-Hmac-Sha256";
/// `X-Shopify-Topic`: the subscription topic (`orders/updated`).
pub const SHOPIFY_TOPIC_HEADER: &str = "X-Shopify-Topic";
/// `X-Shopify-Shop-Domain`: the shop that sent the delivery.
pub const SHOPIFY_SHOP_DOMAIN_HEADER: &str = "X-Shopify-Shop-Domain";
/// `X-Shopify-Event-Id`: the event's identity (stable across redeliveries).
pub const SHOPIFY_EVENT_ID_HEADER: &str = "X-Shopify-Event-Id";
/// `X-Shopify-Webhook-Id`: the delivery's identity (stable across retries).
pub const SHOPIFY_WEBHOOK_ID_HEADER: &str = "X-Shopify-Webhook-Id";
/// `X-Shopify-Triggered-At`: when the event fired.
pub const SHOPIFY_TRIGGERED_AT_HEADER: &str = "X-Shopify-Triggered-At";

/// The pipeline version recorded on Shopify webhook observations.
pub const SHOPIFY_WEBHOOK_PIPELINE: &str = "norn-dataplane.shopify-webhook/1";

/// One shop's webhook configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifySource {
    /// The Norn source name (`shopify`).
    pub source: String,
    /// The shop domain deliveries must come from (`acme.myshopify.com`).
    pub shop_domain: String,
    /// Compliance redaction applied before load, if any.
    pub redaction: Option<RedactionPolicy>,
}

/// The Shopify headers this profile reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShopifyHeaders {
    /// [`SHOPIFY_HMAC_HEADER`].
    pub hmac: Option<String>,
    /// [`SHOPIFY_TOPIC_HEADER`].
    pub topic: Option<String>,
    /// [`SHOPIFY_SHOP_DOMAIN_HEADER`].
    pub shop_domain: Option<String>,
    /// [`SHOPIFY_EVENT_ID_HEADER`].
    pub event_id: Option<String>,
    /// [`SHOPIFY_WEBHOOK_ID_HEADER`].
    pub webhook_id: Option<String>,
    /// [`SHOPIFY_TRIGGERED_AT_HEADER`].
    pub triggered_at: Option<String>,
}

impl ShopifyHeaders {
    /// Read the headers through a case-insensitive lookup supplied by the
    /// host (for example `http::HeaderMap::get`).
    #[must_use]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            hmac: lookup(SHOPIFY_HMAC_HEADER),
            topic: lookup(SHOPIFY_TOPIC_HEADER),
            shop_domain: lookup(SHOPIFY_SHOP_DOMAIN_HEADER),
            event_id: lookup(SHOPIFY_EVENT_ID_HEADER),
            webhook_id: lookup(SHOPIFY_WEBHOOK_ID_HEADER),
            triggered_at: lookup(SHOPIFY_TRIGGERED_AT_HEADER),
        }
    }
}

fn header<'a>(value: Option<&'a String>, field: &'static str) -> Result<&'a str, IngestError> {
    value
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(IngestError::MissingField {
            field,
            expected: "non-empty header",
        })
}

/// Verify and normalize one Shopify webhook delivery.
///
/// `secret` is the app's client secret, `body` the raw request body, and
/// `extracted_at` the host's receipt time in milliseconds.
///
/// # Errors
///
/// A signature failure (checked before anything else is read), a delivery
/// from another shop, missing identity, or a refused observation.
pub fn ingest_shopify_webhook(
    source: &ShopifySource,
    secret: &[u8],
    headers: &ShopifyHeaders,
    body: &[u8],
    extracted_at: u64,
) -> Result<RawObservation, IngestError> {
    verify_hmac_sha256(SHOPIFY_HMAC_HEADER, secret, body, headers.hmac.as_deref())?;
    let shop = header(headers.shop_domain.as_ref(), SHOPIFY_SHOP_DOMAIN_HEADER)?;
    if !shop.eq_ignore_ascii_case(&source.shop_domain) {
        return Err(IngestError::ShopMismatch {
            expected: source.shop_domain.clone(),
            found: shop.to_owned(),
        });
    }
    let topic = header(headers.topic.as_ref(), SHOPIFY_TOPIC_HEADER)?;
    let (resource, action) = topic.split_once('/').ok_or(IngestError::MissingField {
        field: SHOPIFY_TOPIC_HEADER,
        expected: "`resource/action` topic",
    })?;
    let event_id = header(headers.event_id.as_ref(), SHOPIFY_EVENT_ID_HEADER)
        .or_else(|_missing| header(headers.webhook_id.as_ref(), SHOPIFY_WEBHOOK_ID_HEADER))?;
    let kind = if action == "delete" {
        ObservationKind::Delete
    } else {
        ObservationKind::Upsert
    };

    let payload: Value = parse_exact_json(body)?;
    let id = match payload.get("id") {
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) if !text.trim().is_empty() => text.clone(),
        _ => {
            return Err(IngestError::MissingField {
                field: "id",
                expected: "number or string",
            });
        }
    };
    let updated_at = payload.get("updated_at").and_then(Value::as_str);
    let version_input = match (kind, updated_at) {
        (ObservationKind::Upsert, Some(updated_at)) => updated_at,
        _ => header(headers.triggered_at.as_ref(), SHOPIFY_TRIGGERED_AT_HEADER)?,
    };
    let draft = ObservationDraft {
        source: source.source.clone(),
        source_id: format!("{resource}/{id}"),
        event_id: event_id.to_owned(),
        kind,
        source_version: normalize_instant(version_input)?,
        payload,
        extracted_at,
        cursor: None,
        pipeline_version: SHOPIFY_WEBHOOK_PIPELINE.to_owned(),
    };
    Ok(draft.seal(source.redaction.as_ref())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    #[test]
    #[allow(clippy::expect_used, reason = "conformance assertions")]
    fn shopify_ingest_conforms() {
        let source = ShopifySource {
            source: "fixture".into(),
            shop_domain: "fixture.myshopify.com".into(),
            redaction: None,
        };
        let body = br#"{"id":9007199254740993,"updated_at":"2026-11-01T00:00:00Z"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"fixture").expect("key");
        mac.update(body);
        let mut headers = ShopifyHeaders {
            hmac: Some(STANDARD.encode(mac.finalize().into_bytes())),
            topic: Some("orders/updated".into()),
            shop_domain: Some(source.shop_domain.clone()),
            event_id: Some("event/fixture".into()),
            ..ShopifyHeaders::default()
        };
        let obs = ingest_shopify_webhook(&source, b"fixture", &headers, body, 42).expect("signed");
        assert_eq!(obs.source_id, "orders/9007199254740993");
        assert_eq!(obs.source_version, "00001793491200000000");
        assert_eq!(obs.pipeline_version, SHOPIFY_WEBHOOK_PIPELINE);
        let submission = obs.submission().expect("submission");
        assert_eq!(submission.mapping_version, SHOPIFY_WEBHOOK_PIPELINE);
        assert_eq!(submission.payload["payload_digest"], obs.payload_digest);
        let view = crate::Convergence::from_log([&obs, &obs]).expect("replay");
        assert_eq!(
            view.current("fixture", &obs.source_id)
                .expect("record")
                .payload,
            obs.payload
        );
        headers.shop_domain = Some("other.myshopify.com".into());
        assert!(matches!(
            ingest_shopify_webhook(&source, b"fixture", &headers, body, 42),
            Err(IngestError::ShopMismatch { .. })
        ));
        headers.hmac = None;
        assert!(matches!(
            ingest_shopify_webhook(&source, b"fixture", &headers, b"not JSON", 42),
            Err(IngestError::MissingSignature { .. })
        ));
    }
}
