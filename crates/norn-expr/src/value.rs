//! Norn-owned JSON-shaped values exchanged with expressions.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::ExprError;

/// A JSON-shaped value: the context an expression reads and the result it
/// produces.
///
/// Objects are key-ordered ([`BTreeMap`]), so a context has one canonical
/// shape and map iteration inside the engine is deterministic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// An exact decimal number.
    Number(Number),
    /// A UTF-8 string.
    String(String),
    /// An ordered array.
    Array(Vec<Value>),
    /// An object with unique, ordered keys.
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// Builds an object from `(key, value)` pairs; a later duplicate key wins.
    pub fn object<K: Into<String>>(entries: impl IntoIterator<Item = (K, Value)>) -> Self {
        Self::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }
}

impl TryFrom<&norn_ir::RuntimeValue> for Value {
    type Error = ExprError;

    fn try_from(value: &norn_ir::RuntimeValue) -> Result<Self, Self::Error> {
        use norn_ir::RuntimeValue;
        Ok(match value {
            RuntimeValue::Null => Self::Null,
            RuntimeValue::Bool(flag) => Self::Bool(*flag),
            RuntimeValue::Number(number) => Self::Number(number.canonical().parse()?),
            RuntimeValue::String(text) => Self::String(text.clone()),
            RuntimeValue::Array(items) => {
                Self::Array(items.iter().map(Self::try_from).collect::<Result<_, _>>()?)
            }
            RuntimeValue::Object(entries) => Self::Object(
                entries
                    .iter()
                    .map(|(key, item)| Ok((key.clone(), Self::try_from(item)?)))
                    .collect::<Result<BTreeMap<_, _>, ExprError>>()?,
            ),
        })
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<Number> for Value {
    fn from(value: Number) -> Self {
        Self::Number(value)
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Number(Number::from(value))
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Self::Array(value)
    }
}

/// An exact decimal number in canonical form.
///
/// Parsing accepts the JSON number grammar without an exponent
/// (`-?(0|[1-9][0-9]*)(\.[0-9]+)?`) and fails with
/// [`ExprError::InvalidNumber`] unless the engine represents the value
/// exactly: a number is never silently rounded. The canonical form has no
/// trailing fractional zeros and no negative zero, so `"1.50"` and `"1.5"`
/// are the same `Number`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Number(String);

impl Number {
    /// The canonical decimal text, for example `"-12.5"`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wraps text the engine produced; `canonical` must already be canonical.
    pub(crate) fn from_canonical(canonical: String) -> Self {
        Self(canonical)
    }
}

impl From<i64> for Number {
    fn from(value: i64) -> Self {
        Self(value.to_string())
    }
}

impl FromStr for Number {
    type Err = ExprError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: &str| ExprError::InvalidNumber {
            input: input.to_owned(),
            reason: reason.to_owned(),
        };
        if !is_json_decimal(input) {
            return Err(invalid(
                "expected a JSON decimal `-?(0|[1-9][0-9]*)(.[0-9]+)?` without exponent",
            ));
        }
        let canonical = canonicalize(input);
        let engine = crate::convert::canonical_engine_number(input).map_err(|message| {
            ExprError::InvalidNumber {
                input: input.to_owned(),
                reason: message,
            }
        })?;
        if engine != canonical {
            return Err(invalid(
                "not exactly representable by the expression engine",
            ));
        }
        Ok(Self(canonical))
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_json_decimal(input: &str) -> bool {
    let unsigned = input.strip_prefix('-').unwrap_or(input);
    let (int, frac) = match unsigned.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (unsigned, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let int_ok = digits(int) && (int == "0" || !int.starts_with('0'));
    int_ok && frac.is_none_or(digits)
}

/// Canonical text of a grammar-valid decimal: trailing fractional zeros and
/// negative zero removed.
fn canonicalize(input: &str) -> String {
    let (negative, unsigned) = match input.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, input),
    };
    let magnitude = match unsigned.split_once('.') {
        Some((int, frac)) => {
            let frac = frac.trim_end_matches('0');
            if frac.is_empty() {
                int.to_owned()
            } else {
                format!("{int}.{frac}")
            }
        }
        None => unsigned.to_owned(),
    };
    if negative && magnitude.bytes().any(|b| b != b'0' && b != b'.') {
        format!("-{magnitude}")
    } else {
        magnitude
    }
}

#[cfg(test)]
mod tests {
    use super::{Number, canonicalize, is_json_decimal};

    #[test]
    fn grammar() {
        for ok in ["0", "-0", "1", "-12.50", "0.001", "123456789"] {
            assert!(is_json_decimal(ok), "{ok}");
        }
        for bad in [
            "", "-", "01", "1.", ".5", "1e3", "+1", "1_000", " 1", "NaN", "1.2.3",
        ] {
            assert!(!is_json_decimal(bad), "{bad}");
        }
    }

    #[test]
    fn canonical_form() {
        assert_eq!(canonicalize("-0.000"), "0");
        assert_eq!(canonicalize("1.500"), "1.5");
        assert_eq!(canonicalize("-2.0"), "-2");
        assert_eq!(canonicalize("10"), "10");
    }

    #[test]
    fn parse_is_exact_or_rejected() -> Result<(), crate::ExprError> {
        assert_eq!("1.50".parse::<Number>()?, "1.5".parse::<Number>()?);
        assert_eq!("-0".parse::<Number>()?.as_str(), "0");
        // Beyond f64's exact-integer range, still exact.
        assert_eq!(
            "9007199254740993".parse::<Number>()?.as_str(),
            "9007199254740993"
        );
        // Beyond the engine's 96-bit decimal: rejected, never rounded.
        assert!("79228162514264337593543950336".parse::<Number>().is_err());
        assert!("0.00000000000000000000000000001".parse::<Number>().is_err());
        Ok(())
    }
}
