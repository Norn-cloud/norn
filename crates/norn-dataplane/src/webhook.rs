//! Shared webhook ingest pieces: the error type, HMAC-SHA256 signature
//! verification, and source-version normalization.
//!
//! Everything here is pure. The host owns the HTTP listener, reads the raw
//! body bytes and the headers, and hands them in; it then posts the sealed
//! observation to the cell's observe inbox
//! ([`RawObservation::submission`](crate::RawObservation::submission)).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::raw_observation::ObservationError;

/// Why a webhook delivery or sweep page was refused.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// No shared secret is configured; verification fails closed.
    #[error("no webhook secret is configured")]
    MissingSecret,
    /// The signature header is absent or empty.
    #[error("the {header} header is missing")]
    MissingSignature {
        /// The expected header.
        header: &'static str,
    },
    /// The signature header is not base64.
    #[error("the signature is not base64: {reason}")]
    MalformedSignature {
        /// The decoder's message.
        reason: String,
    },
    /// The signature does not match the body.
    #[error("the webhook signature does not match the body")]
    BadSignature,
    /// The secret could not key the MAC.
    #[error("the webhook secret is unusable: {reason}")]
    Key {
        /// The MAC's message.
        reason: String,
    },
    /// The body is not JSON.
    #[error("the body is not JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A required field or header is absent or has the wrong type.
    #[error("required field `{field}` is missing or not a {expected}")]
    MissingField {
        /// The field or header.
        field: &'static str,
        /// The expected JSON type.
        expected: &'static str,
    },
    /// The Frappe `doc_events` name is not one this profile maps.
    #[error("unsupported Frappe doc event `{event}`")]
    UnsupportedEvent {
        /// The event name.
        event: String,
    },
    /// A timestamp could not be parsed.
    #[error("invalid timestamp `{value}`: {reason}")]
    InvalidTimestamp {
        /// The input.
        value: String,
        /// The parser's message.
        reason: String,
    },
    /// The delivery came from a different shop than configured.
    #[error("delivery is for shop `{found}`, expected `{expected}`")]
    ShopMismatch {
        /// The configured shop domain.
        expected: String,
        /// The header value.
        found: String,
    },
    /// The sweep configuration is unusable.
    #[error("invalid sweep configuration: {reason}")]
    InvalidSweep {
        /// Why.
        reason: &'static str,
    },
    /// The resulting observation was refused.
    #[error(transparent)]
    Observation(#[from] ObservationError),
}

/// Verify a base64 HMAC-SHA256 signature of `body` under `secret`, in
/// constant time. Frappe (`X-Frappe-Webhook-Signature`) and Shopify
/// (`X-Shopify-Hmac-Sha256`) both use this scheme over the raw body bytes.
///
/// # Errors
///
/// [`IngestError::MissingSecret`] for an empty secret (fail closed),
/// [`IngestError::MissingSignature`], [`IngestError::MalformedSignature`], or
/// [`IngestError::BadSignature`].
pub fn verify_hmac_sha256(
    header: &'static str,
    secret: &[u8],
    body: &[u8],
    signature: Option<&str>,
) -> Result<(), IngestError> {
    if secret.is_empty() {
        return Err(IngestError::MissingSecret);
    }
    let signature = signature
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(IngestError::MissingSignature { header })?;
    let expected = STANDARD
        .decode(signature)
        .map_err(|error| IngestError::MalformedSignature {
            reason: error.to_string(),
        })?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|error| IngestError::Key {
        reason: error.to_string(),
    })?;
    mac.update(body);
    if mac.verify_slice(&expected).is_err() {
        return Err(IngestError::BadSignature);
    }
    Ok(())
}

/// Normalize a Frappe datetime (`2026-10-01 09:15:02.481233`, fraction
/// optional, `T` separator accepted) to the fixed form
/// `YYYY-MM-DD HH:MM:SS.ffffff`.
///
/// Frappe omits the fraction when it is zero, so the same `modified` value
/// can be spelled two ways; the fixed form makes webhook and sweep event ids
/// identical and keeps lexicographic order equal to time order.
///
/// # Errors
///
/// [`IngestError::InvalidTimestamp`].
pub fn normalize_frappe_datetime(value: &str) -> Result<String, IngestError> {
    let datetime: jiff::civil::DateTime =
        value
            .trim()
            .parse()
            .map_err(|error: jiff::Error| IngestError::InvalidTimestamp {
                value: value.to_owned(),
                reason: error.to_string(),
            })?;
    Ok(format_frappe_datetime(datetime))
}

pub(crate) fn format_frappe_datetime(datetime: jiff::civil::DateTime) -> String {
    let micros = datetime.subsec_nanosecond().checked_div(1000).unwrap_or(0);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{micros:06}",
        datetime.year(),
        datetime.month(),
        datetime.day(),
        datetime.hour(),
        datetime.minute(),
        datetime.second(),
    )
}

/// Normalize an RFC 3339 instant with offset (Shopify `updated_at`,
/// `X-Shopify-Triggered-At`) to zero-padded UTC microseconds since the Unix
/// epoch, so lexicographic order equals time order across offsets.
///
/// # Errors
///
/// [`IngestError::InvalidTimestamp`], including instants before the epoch.
pub fn normalize_instant(value: &str) -> Result<String, IngestError> {
    let invalid = |reason: String| IngestError::InvalidTimestamp {
        value: value.to_owned(),
        reason,
    };
    let instant: jiff::Timestamp = value
        .trim()
        .parse()
        .map_err(|error: jiff::Error| invalid(error.to_string()))?;
    let micros = u64::try_from(instant.as_microsecond())
        .map_err(|error| invalid(format!("before the Unix epoch ({error})")))?;
    Ok(format!("{micros:020}"))
}
