//! Frappe / `ERPNext` `doc_events` webhooks to raw observations (T12-DP-4).
//!
//! # Webhook profile
//!
//! Configure one Frappe **Webhook** per document event, with *Enable Security*
//! on (Frappe then sends `X-Frappe-Webhook-Signature`: base64 HMAC-SHA256 of
//! the body under the webhook secret), request structure JSON, and this body
//! template:
//!
//! ```text
//! {"event": "<doc event>", "doc": {{ doc | tojson }}}
//! ```
//!
//! Frappe converts the document to a dictionary before rendering; `tojson`
//! serializes that dictionary, including parent/child null fields. See the
//! [supported webhook filter](https://docs.frappe.io/framework/user/en/guides/integration/webhooks).
//!
//! For webhook/sweep convergence, the host's sweep principal must have read
//! permission for the document and every parent and child-table field emitted
//! by this template. Field-filtered source profiles cannot promise complete
//! payload convergence and must not be enabled as completeness sweeps.
//!
//! A bare document body (no envelope) is also accepted and treated as
//! `on_update`. The document must carry `doctype`, `name`, and `modified`.
//!
//! # Identity
//!
//! - `source_id` is `{doctype}/{name}`.
//! - An upsert's `event_id` is `{doctype}/{name}@{modified}`; a delete's is
//!   `{doctype}/{name}#deleted@{modified}`, with `modified` normalized
//!   ([`normalize_frappe_datetime`]). The completeness sweep
//!   derives the same ids from the same source state, so a
//!   webhook and a sweep that saw the same version converge to one
//!   observation.
//!
//! DP-FRAPPE-1: authenticate raw bytes before parsing; invalid signatures refuse.
//! DP-FRAPPE-2: signed replay preserves source identity, version and pipeline
//! provenance through sealing, submission and convergence (`frappe_ingest_conforms`).
//! Observations are history, not authorization.

use serde_json::{Map, Value};

use crate::raw_observation::{
    ObservationDraft, ObservationKind, RawObservation, RedactionPolicy, parse_exact_json,
};
use crate::webhook::{IngestError, normalize_frappe_datetime, verify_hmac_sha256};

/// The header carrying Frappe's webhook signature.
pub const FRAPPE_SIGNATURE_HEADER: &str = "X-Frappe-Webhook-Signature";

/// The pipeline version recorded on webhook observations.
pub const FRAPPE_WEBHOOK_PIPELINE: &str = "norn-dataplane.frappe-webhook/1";

/// One Frappe site's webhook configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrappeSource {
    /// The Norn source name (`erpnext`).
    pub source: String,
    /// Compliance redaction applied before load, if any.
    pub redaction: Option<RedactionPolicy>,
}

/// The Frappe document events this profile maps.
const UPSERT_EVENTS: &[&str] = &[
    "after_insert",
    "on_update",
    "on_submit",
    "on_cancel",
    "on_update_after_submit",
    "on_change",
];
const DELETE_EVENTS: &[&str] = &["on_trash", "after_delete"];

/// Map a Frappe doc event name to an observation kind.
///
/// # Errors
///
/// [`IngestError::UnsupportedEvent`].
pub fn frappe_event_kind(event: &str) -> Result<ObservationKind, IngestError> {
    if UPSERT_EVENTS.contains(&event) {
        Ok(ObservationKind::Upsert)
    } else if DELETE_EVENTS.contains(&event) {
        Ok(ObservationKind::Delete)
    } else {
        Err(IngestError::UnsupportedEvent {
            event: event.to_owned(),
        })
    }
}

/// The identity of one Frappe document version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrappeDocVersion {
    pub(crate) source_id: String,
    pub(crate) source_version: String,
}

impl FrappeDocVersion {
    /// `doctype` comes from the document itself (webhooks) or from the
    /// request (sweeps: `/api/resource` rows carry no `doctype` column).
    pub(crate) fn of(doctype: &str, doc: &Map<String, Value>) -> Result<Self, IngestError> {
        let name = string_field(doc, "name")?;
        let modified = normalize_frappe_datetime(string_field(doc, "modified")?)?;
        Ok(Self {
            source_id: format!("{doctype}/{name}"),
            source_version: modified,
        })
    }

    pub(crate) fn event_id(&self, kind: ObservationKind) -> String {
        match kind {
            ObservationKind::Upsert => format!("{}@{}", self.source_id, self.source_version),
            ObservationKind::Delete => {
                format!("{}#deleted@{}", self.source_id, self.source_version)
            }
        }
    }
}

pub(crate) fn string_field<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, IngestError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(IngestError::MissingField {
            field,
            expected: "non-empty string",
        })
}

/// Build the sealed observation for one Frappe document as received.
pub(crate) fn frappe_observation(
    source: &FrappeSource,
    doctype: &str,
    kind: ObservationKind,
    doc: Map<String, Value>,
    extracted_at: u64,
    cursor: Option<Value>,
    pipeline_version: &str,
) -> Result<RawObservation, IngestError> {
    let version = FrappeDocVersion::of(doctype, &doc)?;
    let draft = ObservationDraft {
        source: source.source.clone(),
        event_id: version.event_id(kind),
        source_id: version.source_id,
        kind,
        source_version: version.source_version,
        payload: Value::Object(doc),
        extracted_at,
        cursor,
        pipeline_version: pipeline_version.to_owned(),
    };
    Ok(draft.seal(source.redaction.as_ref())?)
}

/// Verify and normalize one Frappe webhook delivery.
///
/// `body` is the raw request body, `signature` the
/// [`FRAPPE_SIGNATURE_HEADER`] value, and `extracted_at` the host's receipt
/// time in milliseconds.
///
/// # Errors
///
/// A signature failure (checked before the body is parsed), a malformed
/// body, an unsupported event, or a refused observation.
pub fn ingest_frappe_webhook(
    source: &FrappeSource,
    secret: &[u8],
    body: &[u8],
    signature: Option<&str>,
    extracted_at: u64,
) -> Result<RawObservation, IngestError> {
    verify_hmac_sha256(FRAPPE_SIGNATURE_HEADER, secret, body, signature)?;
    let value: Value = parse_exact_json(body)?;
    let Value::Object(mut object) = value else {
        return Err(IngestError::MissingField {
            field: "doc",
            expected: "object",
        });
    };
    let (kind, doc) = match object.remove("doc") {
        Some(Value::Object(doc)) => {
            let event = string_field(&object, "event")?;
            (frappe_event_kind(event)?, doc)
        }
        Some(_) => {
            return Err(IngestError::MissingField {
                field: "doc",
                expected: "object",
            });
        }
        None => (ObservationKind::Upsert, object),
    };
    let doctype = string_field(&doc, "doctype")?.to_owned();
    frappe_observation(
        source,
        &doctype,
        kind,
        doc,
        extracted_at,
        None,
        FRAPPE_WEBHOOK_PIPELINE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    #[test]
    #[allow(clippy::expect_used, reason = "conformance assertions")]
    fn frappe_ingest_conforms() {
        let source = FrappeSource {
            source: "fixture".into(),
            redaction: None,
        };
        let body = br#"{"event":"on_submit","doc":{"doctype":"Order","name":"fixture","modified":"2026-11-01 00:00:00","amount":42}}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"fixture").expect("key");
        mac.update(body);
        let signature = STANDARD.encode(mac.finalize().into_bytes());
        let obs =
            ingest_frappe_webhook(&source, b"fixture", body, Some(&signature), 42).expect("signed");
        assert_eq!(obs.source_id, "Order/fixture");
        assert_eq!(obs.source_version, "2026-11-01 00:00:00.000000");
        assert_eq!(obs.pipeline_version, FRAPPE_WEBHOOK_PIPELINE);
        assert_eq!(obs.extracted_at, 42);
        let submission = obs.submission().expect("submission");
        assert_eq!(submission.mapping_version, FRAPPE_WEBHOOK_PIPELINE);
        assert_eq!(submission.payload["payload_digest"], obs.payload_digest);
        let view = crate::Convergence::from_log([&obs, &obs]).expect("replay");
        assert_eq!(
            view.current("fixture", "Order/fixture")
                .expect("record")
                .payload,
            obs.payload
        );
        assert!(matches!(
            ingest_frappe_webhook(&source, b"fixture", b"not JSON", None, 42),
            Err(IngestError::MissingSignature { .. })
        ));
        assert!(matches!(
            ingest_frappe_webhook(&source, b"other", body, Some(&signature), 42),
            Err(IngestError::BadSignature)
        ));
    }
}
