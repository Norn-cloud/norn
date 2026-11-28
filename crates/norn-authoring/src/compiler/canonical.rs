//! Canonical JSON formatting and Norn1 hashing helpers.
//!
//! Ports `tools/norn-ts/src/canonical.ts` and the `norn1Sha256` helper from
//! `tools/norn-ts/src/compiler/definitions.ts`. Object keys are ordered by
//! `UTF-16` code units to match JavaScript's `Array.prototype.sort()` regardless
//! of `serde_json`'s map backend.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

/// Norn1 domain separator matching `crates/norn-canonical` and `definitions.ts`.
const NORN1_DOMAIN_TAG: &[u8] = b"norn.canonical.json/norn1\0";

/// Maximum safe IEEE-754 integer (`2^53 - 1`), matching JavaScript `Number.MAX_SAFE_INTEGER`.
pub(crate) const MAX_SAFE_INTEGER_I64: i64 = 9_007_199_254_740_991;
/// Minimum safe IEEE-754 integer (`-(2^53 - 1)`), matching JavaScript `Number.MIN_SAFE_INTEGER`.
pub(crate) const MIN_SAFE_INTEGER_I64: i64 = -9_007_199_254_740_991;

/// Error returned when a value violates the canonical JSON data contract.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{path}: {reason}")]
pub struct CanonicalError {
    /// JSONPath-like location of the offending member.
    pub path: String,
    /// Human-readable reason the member is not canonical JSON.
    pub reason: String,
}

impl CanonicalError {
    fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// Compares two strings by their `UTF-16` code unit sequence, matching
/// JavaScript's `Array.prototype.sort()`.
#[must_use]
pub(crate) fn utf16_cmp(left: &str, right: &str) -> Ordering {
    let mut left_units = left.encode_utf16();
    let mut right_units = right.encode_utf16();
    loop {
        match (left_units.next(), right_units.next()) {
            (Some(a), Some(b)) => {
                let ord = a.cmp(&b);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

/// Parses an ECMAScript array index key (`0..=2^32-2` in canonical decimal form).
fn js_array_index(key: &str) -> Option<u32> {
    if key == "0" {
        return Some(0);
    }
    let first = key.bytes().next()?;
    if !(b'1'..=b'9').contains(&first) || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let idx = key.parse::<u32>().ok()?;
    if idx == u32::MAX { None } else { Some(idx) }
}

/// Orders object property keys identically to `JSON.stringify(toJson(value))`
/// in `tools/norn-ts/src/canonical.ts`: ECMAScript array-index keys
/// (`0..=2^32-2`) first in ascending numeric order, followed by remaining keys
/// in `UTF-16` code-unit order.
#[must_use]
pub(crate) fn js_key_cmp(left: &str, right: &str) -> Ordering {
    match (js_array_index(left), js_array_index(right)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => utf16_cmp(left, right),
    }
}

/// Validates and normalizes `value` into canonical JSON form:
///
/// - Rejects `__proto__` object member names.
/// - Normalizes `-0.0` to `0`.
/// - Recursively sorts object members in JavaScript `toJson` enumeration order.
///
/// # Errors
///
/// Returns [`CanonicalError`] if any member name is `__proto__` or a number is
/// non-finite.
pub fn to_canonical_json(value: &Value) -> Result<Value, CanonicalError> {
    to_canonical_at(value, "$")
}

fn to_canonical_at(value: &Value, path: &str) -> Result<Value, CanonicalError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(value.clone()),
        Value::Number(num) => normalize_number(num, path),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                out.push(to_canonical_at(item, &format!("{path}[{idx}]"))?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| js_key_cmp(a.as_str(), b.as_str()));
            let mut out = Map::with_capacity(map.len());
            for key in keys {
                if key == "__proto__" {
                    return Err(CanonicalError::new(
                        format!("{path}.{key}"),
                        "__proto__ is not a portable JSON member name",
                    ));
                }
                if let Some(member) = map.get(key) {
                    let member_path = format!("{path}.{key}");
                    out.insert(key.clone(), to_canonical_at(member, &member_path)?);
                }
            }
            Ok(Value::Object(out))
        }
    }
}

fn normalize_number(num: &Number, path: &str) -> Result<Value, CanonicalError> {
    if let Some(i) = num.as_i64() {
        return Ok(Value::Number(Number::from(i)));
    }
    if let Some(u) = num.as_u64() {
        return Ok(Value::Number(Number::from(u)));
    }
    let Some(f) = num.as_f64() else {
        return Err(CanonicalError::new(
            path,
            format!("non-finite number {num}"),
        ));
    };
    if !f.is_finite() {
        return Err(CanonicalError::new(path, format!("non-finite number {f}")));
    }
    if f.abs().to_bits() == 0 {
        return Ok(Value::Number(Number::from(0_u64)));
    }
    if f.fract().abs().to_bits() == 0
        && f >= -9_007_199_254_740_991.0
        && f <= 9_007_199_254_740_991.0
        && let Ok(i) = format!("{f:.0}").parse::<i64>()
    {
        return Ok(Value::Number(Number::from(i)));
    }
    Number::from_f64(f)
        .map(Value::Number)
        .ok_or_else(|| CanonicalError::new(path, format!("non-finite number {f}")))
}

/// Formats `value` as canonical golden-file JSON: `UTF-16`-sorted keys,
/// two-space indentation, and a single trailing newline, matching
/// `canonicalJson` in `tools/norn-ts/src/canonical.ts`.
///
/// # Errors
///
/// Returns [`CanonicalError`] if `value` contains `__proto__` or a non-finite
/// number.
pub fn canonical_json(value: &Value) -> Result<String, CanonicalError> {
    let normalized = to_canonical_json(value)?;
    let mut out = String::new();
    write_pretty(&normalized, 0, &mut out);
    out.push('\n');
    Ok(out)
}

/// Formats `value` as compact canonical JSON (sorted by `UTF-16` code units,
/// no indentation or trailing newline), matching `JSON.stringify(toJson(value))`.
///
/// # Errors
///
/// Returns [`CanonicalError`] if `value` contains `__proto__` or a non-finite
/// number.
pub fn compact_canonical_json(value: &Value) -> Result<String, CanonicalError> {
    let normalized = to_canonical_json(value)?;
    let mut out = String::new();
    write_compact(&normalized, &mut out);
    Ok(out)
}

fn write_indent(depth: usize, out: &mut String) {
    let spaces = depth.saturating_mul(2);
    for _ in 0..spaces {
        out.push(' ');
    }
}

fn write_escaped_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let code = u32::from(c);
                let _ = write!(out, "\\u{code:04x}");
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Returns the exact UTF-8 byte length of `text` as a quoted, JSON-escaped
/// string matching [`write_escaped_string`].
#[must_use]
pub(crate) fn escaped_json_string_len(text: &str) -> usize {
    let mut len = 2_usize;
    for ch in text.chars() {
        let extra = match ch {
            '"' | '\\' | '\u{0008}' | '\u{000C}' | '\n' | '\r' | '\t' => 2,
            c if u32::from(c) < 0x20 => 6,
            c => c.len_utf8(),
        };
        len = len.saturating_add(extra);
    }
    len
}

/// Returns the exact UTF-8 byte length of `num` formatted by [`write_number`].
#[must_use]
pub(crate) fn js_number_len(num: &Number) -> usize {
    let mut buf = String::new();
    write_number(num, &mut buf);
    buf.len()
}

fn write_js_f64(value: f64, out: &mut String) {
    if value.abs().to_bits() == 0 {
        out.push('0');
        return;
    }
    let mag = if value.is_sign_negative() {
        out.push('-');
        -value
    } else {
        value
    };
    let sci = format!("{mag:e}");
    let Some((mantissa, exp_str)) = sci.split_once('e') else {
        out.push_str(&sci);
        return;
    };
    let Ok(exp) = exp_str.parse::<i32>() else {
        out.push_str(&sci);
        return;
    };
    let mut digits = String::with_capacity(mantissa.len());
    for ch in mantissa.chars() {
        if ch != '.' {
            digits.push(ch);
        }
    }
    let k = i32::try_from(digits.len()).unwrap_or(1);
    let n = exp.saturating_add(1);
    if k <= n && n <= 21 {
        out.push_str(&digits);
        let zeros = usize::try_from(n.saturating_sub(k)).unwrap_or(0);
        for _ in 0..zeros {
            out.push('0');
        }
    } else if n > 0 && n <= 21 {
        let split = usize::try_from(n).unwrap_or(0);
        out.push_str(digits.get(..split).unwrap_or(""));
        out.push('.');
        out.push_str(digits.get(split..).unwrap_or(""));
    } else if n > -6 && n <= 0 {
        out.push_str("0.");
        let zeros = usize::try_from(n.unsigned_abs()).unwrap_or(0);
        for _ in 0..zeros {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let mut chars = digits.chars();
        if let Some(first) = chars.next() {
            out.push(first);
            if k > 1 {
                out.push('.');
                out.push_str(chars.as_str());
            }
        }
        out.push('e');
        if exp >= 0 {
            out.push('+');
            let _ = write!(out, "{exp}");
        } else {
            out.push('-');
            let _ = write!(out, "{}", exp.unsigned_abs());
        }
    }
}

fn write_number(num: &Number, out: &mut String) {
    if let Some(i) = num.as_i64() {
        let _ = write!(out, "{i}");
        return;
    }
    if let Some(u) = num.as_u64() {
        let _ = write!(out, "{u}");
        return;
    }
    if let Some(f) = num.as_f64() {
        write_js_f64(f, out);
    } else {
        let _ = write!(out, "{num}");
    }
}

fn sorted_object_entries(map: &Map<String, Value>) -> Vec<(&str, &Value)> {
    let mut entries: Vec<(&str, &Value)> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
    entries.sort_by(|(left, _), (right, _)| js_key_cmp(left, right));
    entries
}

fn write_pretty(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(num) => write_number(num, out),
        Value::String(text) => write_escaped_string(text, out),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            let next_depth = depth.saturating_add(1);
            for (idx, item) in items.iter().enumerate() {
                write_indent(next_depth, out);
                write_pretty(item, next_depth, out);
                if idx.saturating_add(1) < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            write_indent(depth, out);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            let entries = sorted_object_entries(map);
            let next_depth = depth.saturating_add(1);
            for (idx, (key, val)) in entries.iter().enumerate() {
                write_indent(next_depth, out);
                write_escaped_string(key, out);
                out.push_str(": ");
                write_pretty(val, next_depth, out);
                if idx.saturating_add(1) < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            write_indent(depth, out);
            out.push('}');
        }
    }
}

fn write_compact(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(num) => write_number(num, out),
        Value::String(text) => write_escaped_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_compact(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let entries = sorted_object_entries(map);
            for (idx, (key, val)) in entries.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_escaped_string(key, out);
                out.push(':');
                write_compact(val, out);
            }
            out.push('}');
        }
    }
}

/// Computes the lower-case hexadecimal `SHA-256` digest of `bytes`.
#[must_use]
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Computes the lower-case 64-hex `Norn1` `SHA-256` hash of `value` for the
/// exact subset spelled by `tools/norn-ts/src/compiler/definitions.ts`
/// (`norn1Sha256`), returning `None` when `value` falls outside that subset
/// (for example control characters, non-ASCII object keys, or non-safe-integer
/// numbers).
#[must_use]
pub fn norn1_sha256(value: &Value) -> Option<String> {
    let mut text = String::new();
    write_norn1_subset(value, &mut text)?;
    let mut hasher = Sha256::new();
    hasher.update(NORN1_DOMAIN_TAG);
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Some(hex)
}

fn write_norn1_subset(value: &Value, out: &mut String) -> Option<()> {
    match value {
        Value::Null => {
            out.push_str("null");
            Some(())
        }
        Value::Bool(b) => {
            out.push_str(if *b { "true" } else { "false" });
            Some(())
        }
        Value::Number(num) => {
            let i = num.as_i64()?;
            if !(MIN_SAFE_INTEGER_I64..=MAX_SAFE_INTEGER_I64).contains(&i) {
                return None;
            }
            let _ = write!(out, "{i}");
            Some(())
        }
        Value::String(text) => {
            if text.chars().any(|ch| u32::from(ch) <= 0x1F) {
                return None;
            }
            let normalized: String = text.nfc().collect();
            write_escaped_string(&normalized, out);
            Some(())
        }
        Value::Array(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_norn1_subset(item, out)?;
            }
            out.push(']');
            Some(())
        }
        Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            if keys
                .iter()
                .any(|key| key.bytes().any(|b| !(0x20..=0x7E).contains(&b)))
            {
                return None;
            }
            keys.sort_unstable();
            out.push('{');
            for (idx, key) in keys.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_escaped_string(key, out);
                out.push(':');
                let member = map.get(*key)?;
                write_norn1_subset(member, out)?;
            }
            out.push('}');
            Some(())
        }
    }
}

/// Checks whether `name` is already in `seen`; helper used by schema/definition lowering.
pub(crate) fn insert_unique_str(seen: &mut BTreeSet<String>, value: String) -> bool {
    seen.insert(value)
}

/// Returns `true` if `value` is a valid `snake_case` identifier matching `[a-z][a-z0-9_]*`.
#[must_use]
pub(crate) fn is_snake_ident(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Returns `true` if `value` is a dotted lower-case identifier with at least
/// two `[a-z][a-z0-9_]*` segments.
#[must_use]
pub(crate) fn is_dotted_lower_id(value: &str) -> bool {
    let mut count = 0_usize;
    for segment in value.split('.') {
        if !is_snake_ident(segment) {
            return false;
        }
        count = count.saturating_add(1);
    }
    count >= 2
}

/// Returns `true` if `value` matches `OBJECT_TYPE_NAME`: dotted lower-case
/// namespace segments followed by a `PascalCase` type name (`commerce.SalesOrder`).
#[must_use]
pub(crate) fn is_object_type_name(value: &str) -> bool {
    let Some((namespace, type_name)) = value.rsplit_once('.') else {
        return false;
    };
    if !namespace.split('.').all(is_snake_ident) {
        return false;
    }
    is_pascal_ident(type_name)
}

/// Returns `true` if `value` matches `[A-Z][A-Za-z0-9]*`.
#[must_use]
pub(crate) fn is_pascal_ident(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_uppercase() && bytes.all(|b| b.is_ascii_alphanumeric())
}

/// Computes the `Norn1` `SHA-256` hash of `value` through
/// `norn_canonical::HashProfile::Norn1`, returning `None` if canonical JSON
/// formatting or `norn-canonical` hashing refuses `value`.
#[must_use]
pub fn norn_canonical_hash(value: &Value) -> Option<String> {
    let compact = compact_canonical_json(value).ok()?;
    norn_canonical::HashProfile::Norn1
        .hash_json(compact.as_bytes())
        .ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        canonical_json, compact_canonical_json, escaped_json_string_len, norn_canonical_hash,
        norn1_sha256, to_canonical_json,
    };

    #[test]
    fn canonical_json_sorts_keys_and_normalizes_negative_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        let input = json!({ "b": 1, "a": -0.0, "c": [true, null] });
        let formatted = canonical_json(&input)?;
        assert_eq!(
            formatted,
            "{\n  \"a\": 0,\n  \"b\": 1,\n  \"c\": [\n    true,\n    null\n  ]\n}\n"
        );
        Ok(())
    }

    #[test]
    fn canonical_json_matches_js_number_formatting_and_own_key_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let input = json!({
            "10": 1e-7,
            "2": 1.5e-7,
            "01": 1e-6,
            "4294967295": 1e20,
            "a": 1e21,
            "0": -0.0
        });
        let compact = compact_canonical_json(&input)?;
        assert_eq!(
            compact,
            r#"{"0":0,"2":1.5e-7,"10":1e-7,"01":0.000001,"4294967295":100000000000000000000,"a":1e+21}"#
        );
        Ok(())
    }

    #[test]
    fn escaped_json_string_len_matches_rendered_output() -> Result<(), Box<dyn std::error::Error>> {
        let text = "a\0\u{001f}\n\r\t\"\\é";
        let rendered = compact_canonical_json(&json!(text))?;
        assert_eq!(escaped_json_string_len(text), rendered.len());
        Ok(())
    }

    #[test]
    fn rejects_proto_key() {
        let input = json!({ "__proto__": 1 });
        assert!(to_canonical_json(&input).is_err());
    }

    #[test]
    fn norn1_sha256_matches_norn_canonical() {
        let doc = json!({ "artifactId": "demo.source", "version": "1.0.0", "count": 42 });
        assert_eq!(norn1_sha256(&doc), norn_canonical_hash(&doc));
    }
}
