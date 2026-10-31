//! Effective scalar checks, independent of row selection or backend evaluation.
use crate::{Decimal, Property, ScalarConstraints, Value};
use std::cmp::Ordering;

pub(crate) fn valid(kind: &Property, rules: &ScalarConstraints, value: &Value) -> bool {
    // Native non-decimal check_runtime_value rejects a malformed effective
    // pattern before its kind-specific checks, including for enum/date fields.
    let pattern = rules
        .pattern
        .as_ref()
        .map(|p| regex_lite::RegexBuilder::new(p).size_limit(1 << 20).build());
    if !matches!(kind, Property::Decimal { .. }) && pattern.as_ref().is_some_and(Result::is_err) {
        return false;
    }
    match (kind, value) {
        (Property::Decimal { .. }, Value::Decimal(value)) => {
            let text = decimal_text(*value);
            let Some(value) = Parts::parse(&text) else {
                return false;
            };
            for (bound, prohibited) in [
                (&rules.min, Ordering::Less),
                (&rules.max, Ordering::Greater),
            ] {
                if let Some(bound) = bound {
                    let Some(limit) = Parts::parse(bound) else {
                        return false;
                    };
                    if value.compare(&limit) == prohibited {
                        return false;
                    }
                }
            }
            true
        }
        (Property::Text, Value::Text(text)) => {
            let length = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
            rules.min_length.is_none_or(|min| length >= min)
                && rules.max_length.is_none_or(|max| length <= max)
                && pattern.is_none_or(|compiled| compiled.is_ok_and(|regex| regex.is_match(text)))
        }
        (
            Property::Constrained { .. }
            | Property::Text
            | Property::Enum(_)
            | Property::Date
            | Property::Timestamp
            | Property::Decimal { .. },
            Value::Null | Value::Bool(_) | Value::Decimal(_) | Value::Text(_),
        ) => true,
    }
}
fn decimal_text(value: Decimal) -> String {
    let mut digits = value.mantissa().unsigned_abs().to_string();
    let scale = usize::from(value.scale());
    if scale > 0 {
        while digits.len() <= scale {
            digits.insert(0, '0');
        }
        digits.insert(digits.len().saturating_sub(scale), '.');
    }
    if value.mantissa() < 0 {
        digits.insert(0, '-');
    }
    digits
}
struct Parts<'a> {
    negative: bool,
    integer: &'a str,
    fraction: &'a str,
}
impl<'a> Parts<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let (negative, magnitude) = text.strip_prefix('-').map_or((false, text), |s| (true, s));
        let (integer, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
        if integer.is_empty()
            || !integer.bytes().all(|b| b.is_ascii_digit())
            || (integer.len() > 1 && integer.starts_with('0'))
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || (magnitude.contains('.') && fraction.is_empty())
        {
            return None;
        }
        let fraction = fraction.trim_end_matches('0');
        Some(Self {
            negative: negative && (integer != "0" || !fraction.is_empty()),
            integer,
            fraction,
        })
    }
    fn compare(&self, other: &Self) -> Ordering {
        let magnitude = self
            .integer
            .len()
            .cmp(&other.integer.len())
            .then_with(|| self.integer.cmp(other.integer))
            .then_with(|| self.fraction.cmp(other.fraction));
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => magnitude,
            (true, true) => magnitude.reverse(),
        }
    }
}
