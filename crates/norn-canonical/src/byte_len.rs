//! Zero-allocation UTF-8 byte-length counting for Norn1 canonical JSON.

use serde::Serialize;

use crate::{CanonicalJsonError, JsonValue, canonical_json_of_text, direct, parse, write_value};

/// Destination for Norn1 canonical JSON output: either a materialized
/// [`String`] or a zero-allocation byte counter ([`ByteCounter`]).
pub(crate) trait CanonicalSink {
    fn push_char(&mut self, ch: char);
    fn push_str(&mut self, text: &str);
    fn push_unicode_escape(&mut self, ch: char);
}

impl CanonicalSink for String {
    fn push_char(&mut self, ch: char) {
        self.push(ch);
    }

    fn push_str(&mut self, text: &str) {
        self.push_str(text);
    }

    fn push_unicode_escape(&mut self, ch: char) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = std::fmt::Write::write_fmt(self, format_args!("\\u{:04x}", u32::from(ch)));
    }
}

#[derive(Default)]
struct ByteCounter {
    count: usize,
    overflowed: bool,
}

impl ByteCounter {
    fn add(&mut self, bytes: usize) {
        if let Some(next) = self.count.checked_add(bytes) {
            self.count = next;
        } else {
            self.overflowed = true;
        }
    }

    fn finish(self) -> Result<usize, CanonicalJsonError> {
        if self.overflowed {
            return Err(CanonicalJsonError::Serialize {
                message: "canonical JSON byte length overflow".to_owned(),
            });
        }
        Ok(self.count)
    }
}

impl CanonicalSink for ByteCounter {
    fn push_char(&mut self, ch: char) {
        self.add(ch.len_utf8());
    }

    fn push_str(&mut self, text: &str) {
        self.add(text.len());
    }

    fn push_unicode_escape(&mut self, _ch: char) {
        // C0 controls (`\u{0}..=\u{1f}`) always format as 6 ASCII bytes: `\u00XX`.
        self.add(6);
    }
}

/// Exact UTF-8 byte length of [`crate::canonical_json_of`] for `value`, counted
/// without allocating the final canonical output buffer.
///
/// Enforces every validation and canonicalization rule of
/// [`crate::canonical_json_of`] (nesting depth, NFC duplicate-key checks,
/// number validation and expansion budgets), returning the exact same length
/// and errors as `canonical_json_of(value).map(|bytes| bytes.len())`.
///
/// # Errors
///
/// As [`crate::canonical_json_of`].
///
/// # Panics
///
/// Debug builds only: if the direct byte count and the text path ever produce
/// different lengths, which is a bug in this crate.
pub fn canonical_json_byte_len<T: Serialize + ?Sized>(
    value: &T,
) -> Result<usize, CanonicalJsonError> {
    if let Some(direct) = direct::to_value(value)
        && let Ok(len) = serialized_byte_len(&direct)
    {
        #[cfg(debug_assertions)]
        assert_eq!(
            canonical_json_of_text(value).map(|bytes| bytes.len()),
            Ok(len),
            "the direct Norn1 byte length differs from the text path"
        );
        return Ok(len);
    }
    canonical_json_byte_len_text(value)
}

fn serialized_byte_len(value: &JsonValue) -> Result<usize, CanonicalJsonError> {
    let mut counter = ByteCounter::default();
    let mut number_bytes = 0;
    write_value(value, &mut counter, 0, true, &mut number_bytes)?;
    counter.finish()
}

fn canonical_json_byte_len_text<T: Serialize + ?Sized>(
    value: &T,
) -> Result<usize, CanonicalJsonError> {
    let bytes = serde_json::to_vec(value).map_err(|error| CanonicalJsonError::Serialize {
        message: error.to_string(),
    })?;
    let tree = parse(&bytes)?;
    let mut counter = ByteCounter::default();
    let mut number_bytes = 0;
    write_value(&tree, &mut counter, 0, false, &mut number_bytes)?;
    counter.finish()
}
