//! UI-LIVE-1: the frames of the live-read protocol `norn.live.v1`, as typed
//! contract components.
//!
//! A client opens one WebSocket per page and subscribes to GET routes of the
//! cell. The cell answers each subscription the way it answers that GET, as
//! the subscriber, and again after every commit; only a changed answer is
//! sent. The frames carry nothing the GET would not. These types are the wire
//! shape both hosts send (`crates/norn-cell-rivet/src/live.rs` and
//! `apps/norn-cell-do/subscribe.mjs`), published in the `OpenAPI` document so
//! generated clients type them.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use utoipa::ToSchema;
use utoipa::openapi::schema::{Object, Schema, SchemaType};

/// The WebSocket subprotocol a client asks for.
pub const PROTOCOL: &str = "norn.live.v1";
/// Subscriptions one socket may hold.
pub const MAX_SUBSCRIPTIONS: usize = 8;
/// The largest client frame, in UTF-8 bytes; a larger one closes the socket
/// with 1009.
pub const MAX_MESSAGE: usize = 4096;
/// The longest subscription path, in bytes.
pub const MAX_PATH: usize = 512;

/// Refusal codes a `refused` frame carries.
pub mod codes {
    /// The frame's `id` is not 1 to 64 letters, digits or `_.:-`.
    pub const INVALID_ID: &str = "live.invalid_id";
    /// The path is not one of the cell's GET routes (or is the live route).
    pub const NOT_SUBSCRIBABLE: &str = "live.not_subscribable";
    /// The socket holds [`super::MAX_SUBSCRIPTIONS`] subscriptions already.
    pub const TOO_MANY_SUBSCRIPTIONS: &str = "live.too_many_subscriptions";
    /// The frame is neither `subscribe` nor `unsubscribe`.
    pub const UNKNOWN_MESSAGE: &str = "live.unknown_message";
}

/// A frame the protocol refuses, with its stable code and words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveRefusal {
    /// The frame's `id` is not 1 to 64 letters, digits or `_.:-`.
    InvalidId,
    /// The path is not one of the cell's GET routes.
    NotSubscribable,
    /// The socket holds [`MAX_SUBSCRIPTIONS`] subscriptions already.
    TooManySubscriptions,
    /// The frame is neither `subscribe` nor `unsubscribe`.
    UnknownMessage,
}

impl LiveRefusal {
    /// The stable machine code (`live.*`).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidId => codes::INVALID_ID,
            Self::NotSubscribable => codes::NOT_SUBSCRIBABLE,
            Self::TooManySubscriptions => codes::TOO_MANY_SUBSCRIPTIONS,
            Self::UnknownMessage => codes::UNKNOWN_MESSAGE,
        }
    }

    /// The one-line summary a `refused` frame carries.
    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::InvalidId => "a subscription id is 1 to 64 letters, digits or _.:-".to_owned(),
            Self::NotSubscribable => {
                "a subscription names one of this cell's GET routes".to_owned()
            }
            Self::TooManySubscriptions => {
                format!("at most {MAX_SUBSCRIPTIONS} subscriptions per socket")
            }
            Self::UnknownMessage => "messages are subscribe or unsubscribe".to_owned(),
        }
    }

    /// The `refused` frame for the client frame `id` (none when it named no valid id).
    #[must_use]
    pub fn frame(self, id: Option<&str>) -> LiveServerMessage {
        LiveServerMessage::refused(id, self.code(), self.message())
    }
}

/// A frame a client sends on a live socket (`norn.live.v1`). A frame larger
/// than 4096 UTF-8 bytes closes the socket with 1009; one that is not JSON
/// closes it with 1007.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveClientMessage {
    /// Answer `path` now and after every commit that changes its answer.
    /// Subscribing again under a held `id` replaces that subscription.
    Subscribe {
        /// The client's name for the subscription: 1 to 64 letters, digits
        /// or `_.:-`, unique on its socket.
        id: String,
        /// One of the cell's GET routes with its query (`/v1/…?…`), at most
        /// 512 bytes; never the live route itself.
        path: String,
    },
    /// Stop answering the subscription `id`.
    Unsubscribe {
        /// The subscription's id.
        id: String,
    },
}

/// A frame the cell sends on a live socket (`norn.live.v1`). A token the cell
/// no longer admits closes the socket with 4401; the client reconnects with a
/// fresh one.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveServerMessage {
    /// The route's answer, on subscribe and on every change.
    Snapshot {
        /// The subscription's id.
        id: String,
        /// The HTTP status the GET answers with now (a refusal keeps its own).
        status: u16,
        /// A digest of the status and the answer; a frame is sent only when it
        /// changes.
        digest: String,
        /// The GET's own document, byte for byte (exact decimals stay number
        /// tokens), or null when the answer is not JSON.
        #[schema(schema_with = body_schema, required = true)]
        body: Option<Box<RawValue>>,
    },
    /// The protocol refused a frame: no subscription was made or changed.
    Refused {
        /// The refused subscription's id; null when the frame named no valid
        /// id.
        #[schema(required = true, nullable = true)]
        id: Option<String>,
        /// Stable machine code: `live.invalid_id`, `live.not_subscribable`,
        /// `live.too_many_subscriptions` or `live.unknown_message`.
        code: String,
        /// One-line summary.
        message: String,
    },
}

impl LiveServerMessage {
    /// A refusal of the frame `id` with `code`.
    #[must_use]
    pub fn refused(id: Option<&str>, code: &str, message: impl Into<String>) -> Self {
        Self::Refused {
            id: id.map(str::to_owned),
            code: code.to_owned(),
            message: message.into(),
        }
    }

    /// The answer `body` (the GET's response text) to subscription `id`;
    /// a body that is not JSON is sent as null.
    #[must_use]
    pub fn snapshot(id: &str, status: u16, digest: &str, body: String) -> Self {
        Self::Snapshot {
            id: id.to_owned(),
            status,
            digest: digest.to_owned(),
            body: RawValue::from_string(body).ok(),
        }
    }

    /// The frame's text.
    ///
    /// # Errors
    /// Returns a serialization failure.
    pub fn to_text(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

fn body_schema() -> Schema {
    let mut schema = Object::with_type(SchemaType::AnyValue);
    schema.description = Some(
        "The GET's own document, byte for byte (exact decimals stay number tokens), \
         or null when the answer is not JSON."
            .to_owned(),
    );
    Schema::Object(schema)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed frames")]
mod tests {
    use super::{LiveClientMessage, LiveRefusal, LiveServerMessage, codes};

    /// The typed frames are the wire frames both hosts send.
    #[test]
    fn frames_serialize_to_the_protocol_wire_shape() {
        let snapshot = LiveServerMessage::snapshot(
            "s1",
            200,
            "abc",
            r#"{"total":12345678901234567890.123456789}"#.to_owned(),
        );
        assert_eq!(
            snapshot.to_text().unwrap(),
            r#"{"type":"snapshot","id":"s1","status":200,"digest":"abc","body":{"total":12345678901234567890.123456789}}"#
        );
        let opaque = LiveServerMessage::snapshot("s1", 502, "abc", "not json".to_owned());
        assert_eq!(
            opaque.to_text().unwrap(),
            r#"{"type":"snapshot","id":"s1","status":502,"digest":"abc","body":null}"#
        );
        let refused =
            LiveServerMessage::refused(None, codes::INVALID_ID, "a subscription id is 1 to 64");
        assert_eq!(
            refused.to_text().unwrap(),
            r#"{"type":"refused","id":null,"code":"live.invalid_id","message":"a subscription id is 1 to 64"}"#
        );
        assert_eq!(
            LiveRefusal::TooManySubscriptions
                .frame(Some("s9"))
                .to_text()
                .unwrap(),
            r#"{"type":"refused","id":"s9","code":"live.too_many_subscriptions","message":"at most 8 subscriptions per socket"}"#
        );
        let subscribe: LiveClientMessage =
            serde_json::from_str(r#"{"type":"subscribe","id":"s1","path":"/v1/runs/r"}"#).unwrap();
        assert_eq!(
            subscribe,
            LiveClientMessage::Subscribe {
                id: "s1".to_owned(),
                path: "/v1/runs/r".to_owned()
            }
        );
        assert_eq!(
            serde_json::to_string(&LiveClientMessage::Unsubscribe {
                id: "s1".to_owned()
            })
            .unwrap(),
            r#"{"type":"unsubscribe","id":"s1"}"#
        );
    }
}
