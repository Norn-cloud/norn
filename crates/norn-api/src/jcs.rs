//! RFC 8785 JSON Canonicalization Scheme (JCS), for connector tool manifest
//! digests shared with Studio's `canonicalize@5.1.0`.
//!
//! This is deliberately not `RuntimeValue::canonical_json`: JCS reads every
//! number as an IEEE 754 double and spells it as ECMAScript's
//! `Number.prototype.toString` does, sorts object members by their UTF-16
//! code units, and escapes strings as `JSON.stringify`. The golden vectors in
//! `tools/norn-ts/src/builder/fixtures/binding-manifest-digest.vectors.json`
//! pin both sides.

use norn_ir::canonical::{JsonValue, parse_json_verbatim};
use serde_json::Value;

/// JCS text of `value`.
///
/// # Errors
/// A number that is not finite as a double.
pub fn canonicalize(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
}

/// JCS text of the JSON document `text`, parsed with every number as the
/// double nearest its decimal text ([`read`]).
///
/// # Errors
/// As [`read`], or a number that is not finite as a double.
pub fn canonicalize_text(text: &str) -> Result<String, String> {
    canonicalize(&read(text)?)
}

/// The JSON document `text` as JCS reads it: every number the double
/// nearest its decimal text, strings and member names verbatim.
///
/// # Errors
/// Malformed JSON (including duplicate member names, which I-JSON forbids),
/// or a number that is not finite as a double.
pub fn read(text: &str) -> Result<Value, String> {
    doubles(&parse_json_verbatim(text).map_err(|error| error.to_string())?)
}

/// `value` with every number the double nearest its exact decimal text, read
/// by the standard library's correctly rounded parse. `serde_json`'s default
/// float parser can land one ULP off (RFC 8785 `values.json`:
/// `333333333.33333329` must be `333333333.3333333`, #1429). Strings and
/// member names stay verbatim: JCS never normalizes them.
fn doubles(value: &JsonValue) -> Result<Value, String> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(flag) => Value::Bool(*flag),
        JsonValue::Number(text) => {
            let double: f64 = text
                .parse()
                .map_err(|_unparsed| format!("{text} is not a number"))?;
            Value::Number(
                serde_json::Number::from_f64(double)
                    .ok_or_else(|| format!("{text} is not a finite number"))?,
            )
        }
        JsonValue::String(text) => Value::String(text.clone()),
        JsonValue::Array(items) => {
            Value::Array(items.iter().map(doubles).collect::<Result<_, _>>()?)
        }
        JsonValue::Object(members) => Value::Object(
            members
                .iter()
                .map(|(name, member)| Ok((name.clone(), doubles(member)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}

fn write(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            let double = number
                .as_f64()
                .ok_or_else(|| format!("{number} is not a double"))?;
            out.push_str(&number_text(double)?);
        }
        Value::String(text) => {
            out.push_str(&serde_json::to_string(text).map_err(|error| error.to_string())?);
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Object(members) => {
            let mut keys: Vec<&String> = members.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).map_err(|error| error.to_string())?);
                out.push(':');
                if let Some(member) = members.get(key) {
                    write(member, out)?;
                }
            }
            out.push('}');
        }
    }
    Ok(())
}

/// ECMAScript `Number.prototype.toString` of a finite double (ECMA-262
/// `Number::toString`, radix 10).
fn number_text(double: f64) -> Result<String, String> {
    if !double.is_finite() {
        return Err(format!("{double} is not a finite number"));
    }
    if double == 0.0 {
        // -0 spells as 0.
        return Ok("0".to_owned());
    }
    // Rust's `{:e}` gives the shortest digits that round-trip: `d[.ddd]e[-]x`.
    let scientific = format!("{:e}", double.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .ok_or_else(|| format!("unexpected float form {scientific}"))?;
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i64 = exponent
        .parse()
        .map_err(|_unparsed| format!("unexpected exponent in {scientific}"))?;
    let k = i64::try_from(digits.len()).map_err(|_overflow| "too many digits".to_owned())?;
    // value = 0.d1…dk × 10^n. A double's exponent is within ±324 and it has
    // at most 17 digits, so no operation below saturates.
    let n = exponent.saturating_add(1);
    let zeros = |count: i64| "0".repeat(usize::try_from(count).unwrap_or(0));
    let split = |at: i64| {
        let at = usize::try_from(at).unwrap_or(0).min(digits.len());
        digits.split_at(at)
    };
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", zeros(n.saturating_sub(k)))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = split(n);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", zeros(n.saturating_neg()))
    } else {
        let (first, rest) = split(1);
        let shown = n.saturating_sub(1);
        let sign = if shown < 0 { '-' } else { '+' };
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        format!("{first}{fraction}e{sign}{}", shown.unsigned_abs())
    };
    Ok(if double < 0.0 {
        format!("-{body}")
    } else {
        body
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed vectors")]
mod tests {
    use super::*;

    #[test]
    fn numbers_spell_as_ecmascript() {
        for (double, text) in [
            (1.0, "1"),
            (1e3, "1000"),
            (-0.0, "0"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1e-7, "1e-7"),
            (1e-6, "0.000001"),
            (123.456, "123.456"),
            (-1.5e-10, "-1.5e-10"),
            (9_007_199_254_740_992.0, "9007199254740992"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
        ] {
            assert_eq!(number_text(double).unwrap(), text, "{double}");
        }
    }

    #[test]
    fn numbers_read_as_the_nearest_double() {
        // RFC 8785 `values.json` (#1429): serde_json's default parser lands
        // one ULP below the nearest double here.
        assert_eq!(
            canonicalize_text("[333333333.33333329]").unwrap(),
            "[333333333.3333333]"
        );
        let binding = read("{\"minimum\":333333333.33333329}").unwrap();
        assert_eq!(
            canonicalize(&binding).unwrap(),
            "{\"minimum\":333333333.3333333}"
        );
        assert!(
            read("{\"a\":1,\"a\":2}").is_err(),
            "I-JSON forbids duplicate names"
        );
    }

    /// #1526 S3: an exponent is read by the binary64 parse, never expanded,
    /// so values JavaScript reads as 0 or a plain double are read so here.
    #[test]
    fn exponents_are_read_as_doubles_without_expansion() {
        assert_eq!(canonicalize_text("[1e-1048576]").unwrap(), "[0]");
        assert_eq!(canonicalize_text("[-1e-1048576]").unwrap(), "[0]");
        assert_eq!(canonicalize_text("[1E+2,25e-1]").unwrap(), "[100,2.5]");
        assert!(canonicalize_text("[1e1048576]").is_err(), "not finite");
        assert!(canonicalize_text("[01]").is_err(), "the token is validated");
        assert!(
            canonicalize_text("{\"a\":1e-1048576,\"a\":2}").is_err(),
            "duplicate names are still refused"
        );
    }

    #[test]
    fn members_sort_by_utf16_code_units() {
        // U+FB33 sorts after U+1F600 in UTF-16 (0xD83D < 0xFB33), unlike in UTF-8.
        let text = "{\"\\uFB33\":1,\"\\uD83D\\uDE00\":2,\"\\u00E9\":3,\"\\u20AC\":4,\"a\":5}";
        assert_eq!(
            canonicalize_text(text).unwrap(),
            "{\"a\":5,\"\u{e9}\":3,\"\u{20ac}\":4,\"\u{1f600}\":2,\"\u{fb33}\":1}"
        );
    }
}
