//! RFC 8785 JSON Canonicalization Scheme (JCS), for connector tool manifest
//! digests shared with Studio's `canonicalize@5.1.0`.
//!
//! This is deliberately not `RuntimeValue::canonical_json`: JCS reads every
//! number as an IEEE 754 double and spells it as ECMAScript's
//! `Number.prototype.toString` does, sorts object members by their UTF-16
//! code units, and escapes strings as `JSON.stringify`. The golden vectors in
//! `tools/norn-ts/src/builder/fixtures/binding-manifest-digest.vectors.json`
//! pin both sides.

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

/// JCS text of the JSON document `text`, parsed with every number as a
/// double.
///
/// # Errors
/// Malformed JSON, or a number that is not finite as a double.
pub fn canonicalize_text(text: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    canonicalize(&value)
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
            (9_007_199_254_740_993.0, "9007199254740992"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
        ] {
            assert_eq!(number_text(double).unwrap(), text, "{double}");
        }
    }

    #[test]
    fn members_sort_by_utf16_code_units() {
        // U+FB33 sorts after U+1F600 in UTF-16 (0xD83D < 0xFB33), unlike in UTF-8.
        let text = r#"{"דּ":1,"😀":2,"é":3,"€":4,"a":5}"#;
        assert_eq!(
            canonicalize_text(text).unwrap(),
            "{\"a\":5,\"é\":3,\"€\":4,\"😀\":2,\"\u{FB33}\":1}"
        );
    }
}
