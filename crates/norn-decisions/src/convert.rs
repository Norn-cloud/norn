//! The only place Norn values meet zen `Variable`s, plus the exact-number
//! bridges to canonical JSON.
//!
//! Numbers never pass through `f64` or serde here: a decimal travels as its
//! canonical text into the engine's decimal (`str::parse`) and back
//! (`normalize().to_string()`). At the current fork pin, serializing an engine
//! value with serde truncates fractions (upstream #530, ledger PR #2858), so
//! engine output is converted by hand. The engine's decimal type is never
//! named; it is reached through `Variable::Number` inference, so
//! `norn-decisions` stays within its SKELETON.md dependency list.

use std::collections::BTreeMap;

use norn_expr::{Number, Value};
use norn_ir::canonical::{JsonValue as CanonicalJson, nfc};
use norn_ir::{Decimal, JsonValue, RuntimeValue};
use zen_engine::Variable;

use crate::DecisionError;

/// Converts a Norn value to the engine's form. Strings and keys are
/// NFC-normalized, the form Norn1 canonicalizes them to (as `norn-expr` does).
/// Objects are inserted in key order, and the vendored engine's ordered maps
/// keep that order at every size.
pub(crate) fn to_variable(value: &Value) -> Result<Variable, DecisionError> {
    Ok(match value {
        Value::Null => Variable::Null,
        Value::Bool(flag) => Variable::Bool(*flag),
        Value::Number(number) => Variable::Number(number.as_str().parse().map_err(|err| {
            DecisionError::InvalidNumber {
                input: number.as_str().to_owned(),
                reason: format!("{err}"),
            }
        })?),
        Value::String(text) => Variable::String(Variable::key(&nfc(text))),
        Value::Array(items) => Variable::from_array(
            items
                .iter()
                .map(to_variable)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Object(entries) => {
            // Keys are inserted literally (never as dot paths), in key order.
            let object = Variable::empty_object();
            if let Variable::Object(map) = &object {
                let mut map = map.borrow_mut();
                for (key, item) in entries {
                    map.insert(Variable::key(&nfc(key)), to_variable(item)?);
                }
            }
            object
        }
    })
}

/// Converts engine output back, preserving exact decimals and canonical S5
/// temporal strings. Other dynamic values have no Norn representation.
pub(crate) fn from_variable(variable: &Variable) -> Result<Value, DecisionError> {
    Ok(match variable {
        Variable::Null => Value::Null,
        Variable::Bool(flag) => Value::Bool(*flag),
        Variable::Number(decimal) => Value::Number(number(&decimal.normalize().to_string())?),
        Variable::String(text) => Value::String(text.to_string()),
        Variable::Array(items) => Value::Array(
            items
                .borrow()
                .iter()
                .map(from_variable)
                .collect::<Result<_, _>>()?,
        ),
        Variable::Object(entries) => Value::Object(
            entries
                .borrow()
                .iter()
                .map(|(key, item)| Ok((key.to_string(), from_variable(item)?)))
                .collect::<Result<BTreeMap<_, _>, DecisionError>>()?,
        ),
        // Only the closed S5 registry can produce these dynamic kinds: input
        // values are JSON-shaped, and function/custom nodes are refused.
        // zen-engine re-exports Variable, but not the private Temporal type.
        Variable::Dynamic(temporal) if matches!(temporal.type_name(), "date" | "timestamp") => {
            let encoded = temporal.to_value();
            let text = encoded
                .as_str()
                .ok_or_else(|| DecisionError::UnsupportedValue {
                    type_name: variable.type_name().to_owned(),
                })?;
            Value::String(text.to_owned())
        }
        Variable::Dynamic(_) => {
            return Err(DecisionError::UnsupportedValue {
                type_name: variable.type_name().to_owned(),
            });
        }
    })
}

fn number(text: &str) -> Result<Number, DecisionError> {
    text.parse::<Number>()
        .map_err(|error| DecisionError::InvalidNumber {
            input: text.to_owned(),
            reason: error.to_string(),
        })
}

/// Converts a canonical (Norn1) JSON value, the kernel's exact-number wire
/// form, to a [`Value`].
///
/// # Errors
///
/// [`DecisionError::InvalidNumber`] when a number is outside the engine's
/// exact decimal range: it is rejected, never rounded.
pub fn value_from_canonical(value: &CanonicalJson) -> Result<Value, DecisionError> {
    Ok(match value {
        CanonicalJson::Null => Value::Null,
        CanonicalJson::Bool(flag) => Value::Bool(*flag),
        CanonicalJson::Number(text) => Value::Number(number(text)?),
        CanonicalJson::String(text) => Value::String(text.clone()),
        CanonicalJson::Array(items) => Value::Array(
            items
                .iter()
                .map(value_from_canonical)
                .collect::<Result<_, _>>()?,
        ),
        CanonicalJson::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, item)| Ok((key.clone(), value_from_canonical(item)?)))
                .collect::<Result<BTreeMap<_, _>, DecisionError>>()?,
        ),
    })
}

/// Converts a [`Value`] to canonical (Norn1) JSON, keeping every number's
/// exact decimal text.
#[must_use]
pub fn value_to_canonical(value: &Value) -> CanonicalJson {
    match value {
        Value::Null => CanonicalJson::Null,
        Value::Bool(flag) => CanonicalJson::Bool(*flag),
        Value::Number(number) => CanonicalJson::Number(number.as_str().to_owned()),
        Value::String(text) => CanonicalJson::String(text.clone()),
        Value::Array(items) => CanonicalJson::Array(items.iter().map(value_to_canonical).collect()),
        Value::Object(entries) => CanonicalJson::Object(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), value_to_canonical(item)))
                .collect(),
        ),
    }
}

/// Converts a decision output value to an exact runtime value for the
/// canonical [`norn_ir::DecisionOutput`] (#2770): every decimal is carried
/// exactly; nothing is narrowed to `f64`.
///
/// # Errors
///
/// [`DecisionError::InvalidNumber`] if a number is outside the runtime decimal
/// range (the same range as the engine's, so this does not happen for an
/// engine value).
pub fn value_to_runtime(value: &Value) -> Result<RuntimeValue, DecisionError> {
    Ok(match value {
        Value::Null => RuntimeValue::Null,
        Value::Bool(flag) => RuntimeValue::Bool(*flag),
        Value::Number(number) => {
            let decimal =
                Decimal::parse(number.as_str()).map_err(|error| DecisionError::InvalidNumber {
                    input: number.as_str().to_owned(),
                    reason: error.to_string(),
                })?;
            RuntimeValue::Number(decimal)
        }
        Value::String(text) => RuntimeValue::String(text.clone()),
        Value::Array(items) => RuntimeValue::Array(
            items
                .iter()
                .map(value_to_runtime)
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(entries) => RuntimeValue::Object(
            entries
                .iter()
                .map(|(key, item)| Ok((key.clone(), value_to_runtime(item)?)))
                .collect::<Result<BTreeMap<_, _>, DecisionError>>()?,
        ),
    })
}

/// Converts an exact runtime value to a Norn value (a slice, an event, or an
/// exact output default), exactly.
///
/// # Errors
///
/// [`DecisionError::InvalidNumber`] if a number is outside the engine's range.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
pub fn value_from_runtime(value: &RuntimeValue) -> Result<Value, DecisionError> {
    Value::try_from(value).map_err(|error| DecisionError::InvalidNumber {
        input: match &error {
            norn_expr::ExprError::InvalidNumber { input, .. } => input.clone(),
            _ => String::new(),
        },
        reason: error.to_string(),
    })
}

/// Converts an IR JSON value (an output default) to a [`Value`].
///
/// IR JSON carries numbers as `f64`, which cannot recover an authored decimal
/// in general (`10000000000000001` and `1.00000000000000001` both decode to a
/// different value), so a numeric default is refused here; admission takes it
/// from the artifact's source text instead
/// ([`crate::AdmittedDecision::admit_with_defaults`]). Every other JSON value
/// converts exactly.
pub(crate) fn value_from_ir(value: &JsonValue) -> Result<Value, DecisionError> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(flag) => Value::Bool(*flag),
        JsonValue::Number(float) => {
            return Err(DecisionError::InvalidNumber {
                input: float.to_string(),
                reason: "numeric output defaults are not admitted: IR JSON carries numbers as f64, so the authored decimal is not recoverable".to_owned(),
            });
        }
        JsonValue::String(text) => Value::String(text.clone()),
        JsonValue::Array(items) => {
            Value::Array(items.iter().map(value_from_ir).collect::<Result<_, _>>()?)
        }
        JsonValue::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, item)| Ok((key.clone(), value_from_ir(item)?)))
                .collect::<Result<BTreeMap<_, _>, DecisionError>>()?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{from_variable, to_variable, value_from_canonical, value_to_canonical};
    use norn_expr::{Number, Value};
    use norn_ir::canonical::JsonValue as CanonicalJson;

    fn num(text: &str) -> Value {
        Value::Number(text.parse::<Number>().unwrap())
    }

    #[test]
    fn engine_round_trip_keeps_fractions_and_big_integers() {
        let value = Value::object([
            ("half", num("1.5")),
            ("tiny", num("0.0000001")),
            ("big", num("9007199254740993")),
            ("neg", num("-12.25")),
            ("s", Value::from("é")),
            ("a", Value::Array(vec![Value::Null, Value::Bool(true)])),
        ]);
        assert_eq!(from_variable(&to_variable(&value).unwrap()).unwrap(), value);
    }

    #[test]
    fn canonical_round_trip_is_exact() {
        let canonical = CanonicalJson::Object(
            [
                ("a".to_owned(), CanonicalJson::Number("0.1".to_owned())),
                (
                    "b".to_owned(),
                    CanonicalJson::Number("79228162514264337593543950335".to_owned()),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let value = value_from_canonical(&canonical).unwrap();
        assert_eq!(value_to_canonical(&value), canonical);
        // Beyond the engine's 96-bit decimal: rejected, not rounded.
        assert!(
            value_from_canonical(&CanonicalJson::Number(
                "79228162514264337593543950336".to_owned()
            ))
            .is_err()
        );
    }
}
