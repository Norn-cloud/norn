//! Translatable user-facing text, shaped as GT shapes it (#1762).
//!
//! Text a person reads is either locale-keyed ([`LocaleText`]) or a source message
//! ([`WidgetMessage`]) whose stable id is [`message_id`] of its text and context; a host draws a
//! message through its translations, keyed by that id, and falls back to the source text. The
//! vocabulary is shared by every surface that shows text (labels and Widget UX here; Studio's own
//! pages through `tools/norn-ts`), so it lives apart from any one of them. Wire shapes and schema
//! names are those the types have always had; checks that an id matches its text are the checkers'.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::present_non_null;

/// Locale-keyed user-facing text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct LocaleText(pub BTreeMap<String, String>);

/// User-facing text in a Widget UX contract: a source message, or locale-keyed text.
///
/// A message is shaped as GT shapes one (#1762): its source text, an optional context that tells
/// two uses of one text apart, and a stable id, [`message_id`] of both. A host draws it through its
/// translations, keyed by the id, and falls back to the source text. The checker refuses an id
/// that is not the hash of its text and context, so no id is hand-written or left stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum WidgetText {
    /// `{ "msg": { "id", "source", "context"? } }`.
    Message(WidgetMessageText),
    /// `{ "<locale>": "<text>", … }`.
    Locales(LocaleText),
}

/// The wire form of a message: one member, `msg`, so no locale map can be read as one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetMessageText {
    /// The message.
    pub msg: WidgetMessage,
}

/// One source message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetMessage {
    /// [`message_id`] of `source` and `context`.
    pub id: String,
    /// The text as written, in the contract's source language.
    pub source: String,
    /// What tells this use of the text apart from another use of the same text.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub context: Option<String>,
}

impl WidgetText {
    /// Every text as written: each locale's, or a message's source text under `"source"`.
    #[must_use]
    pub fn entries(&self) -> Vec<(&str, &str)> {
        match self {
            Self::Message(text) => vec![("source", text.msg.source.as_str())],
            Self::Locales(text) => text
                .0
                .iter()
                .map(|(locale, value)| (locale.as_str(), value.as_str()))
                .collect(),
        }
    }

    /// Whether it holds no text at all (an empty locale map).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Message(_) => false,
            Self::Locales(text) => text.0.is_empty(),
        }
    }

    /// Whether every text it holds is blank, or it holds none.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.is_empty()
            || self
                .entries()
                .iter()
                .any(|(_, text)| text.trim().is_empty())
    }
}

/// A message's stable id: `m` and the 64-bit FNV-1a hash, in lowercase hex, of the UTF-8 bytes of
/// its context (empty when it has none), U+0000, and its source text. `tools/norn-ts`'s
/// `messageId` computes the same id.
///
/// `None` when the text or context holds U+0000: the separator would then be ambiguous, and two
/// different messages (`"a\0b"` in `"c"`, `"b"` in `"c\0a"`) would share one id.
#[must_use]
pub fn message_id(source: &str, context: Option<&str>) -> Option<String> {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    if source.contains('\0') || context.is_some_and(|context| context.contains('\0')) {
        return None;
    }
    let bytes = context
        .unwrap_or_default()
        .bytes()
        .chain(std::iter::once(0))
        .chain(source.bytes());
    let hash = bytes.fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    });
    Some(format!("m{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use super::{WidgetText, message_id};

    #[test]
    fn a_message_id_is_the_hash_of_its_text_and_context() {
        // `tools/norn-ts`'s `messageId` holds the same rows (test/studio-pages.test.ts).
        assert_eq!(
            message_id("Reading the record", None).as_deref(),
            Some("mab651c50e3d7818b")
        );
        assert_eq!(
            message_id("Reading the record", Some("")),
            message_id("Reading the record", None)
        );
        assert_ne!(
            message_id("Close", Some("a dialog")),
            message_id("Close", None)
        );
        // U+0000 separates the context from the text, so neither may hold it.
        assert_eq!(message_id("b\0c", Some("a")), None);
        assert_eq!(message_id("c", Some("a\0b")), None);
    }

    #[test]
    fn a_message_and_a_locale_map_read_as_themselves() {
        let message: WidgetText =
            serde_json::from_str(r#"{"msg":{"id":"m1","source":"Retry"}}"#).expect("message");
        assert!(matches!(message, WidgetText::Message(_)));
        assert_eq!(message.entries(), vec![("source", "Retry")]);
        // A locale whose tag is `msg` is still a locale.
        let locales: WidgetText =
            serde_json::from_str(r#"{"msg":"Retry","ar":"إعادة"}"#).expect("locales");
        assert!(matches!(locales, WidgetText::Locales(_)));
        assert!(!locales.is_blank());
        let blank: WidgetText = serde_json::from_str(r#"{"en":" "}"#).expect("blank");
        assert!(blank.is_blank());
    }
}
