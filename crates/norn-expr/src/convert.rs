//! The only place Norn values meet zen `Variable`s. Nothing here is public.
//!
//! The engine's decimal type is never named: it is reached through
//! `Variable::Number` inference, so `norn-expr` stays within its SKELETON.md
//! dependency list (`zen-expression`, `norn-ir`).

use zen_expression::Variable;
use zen_expression::temporal::Temporal;
use zen_expression::variable::VariableMap;

use norn_ir::canonical::nfc;

use crate::{ExprError, Number, Value};

/// Parses a grammar-valid decimal with the engine and returns the engine's
/// canonical text, so callers can detect silent rounding.
pub(crate) fn canonical_engine_number(input: &str) -> Result<String, String> {
    match Variable::Number(input.parse().map_err(|err| format!("{err}"))?) {
        Variable::Number(decimal) => Ok(decimal.normalize().to_string()),
        Variable::Null
        | Variable::Bool(_)
        | Variable::String(_)
        | Variable::Array(_)
        | Variable::Object(_)
        | Variable::Dynamic(_) => Err("engine did not produce a number".to_owned()),
    }
}

/// Converts a Norn value to the engine's form. Strings and keys are
/// NFC-normalized, the form Norn1 canonicalizes them to, so every string the
/// evaluator sees is at most as long as its canonical bytes (the static cost
/// estimate's slice premise; T1-EXPR-4).
pub(crate) fn to_variable(value: &Value) -> Result<Variable, ExprError> {
    Ok(match value {
        Value::Null => Variable::Null,
        Value::Bool(b) => Variable::Bool(*b),
        Value::Number(n) => {
            Variable::Number(n.as_str().parse().map_err(|err| ExprError::InvalidNumber {
                input: n.as_str().to_owned(),
                reason: format!("{err}"),
            })?)
        }
        Value::String(s) => Variable::String(Variable::key(&nfc(s))),
        Value::Array(items) => Variable::from_array(
            items
                .iter()
                .map(to_variable)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Object(entries) => {
            let mut map = VariableMap::with_capacity(entries.len());
            for (key, item) in entries {
                map.insert(Variable::key(&nfc(key)), to_variable(item)?);
            }
            Variable::from_object(map)
        }
    })
}

pub(crate) fn from_variable(variable: &Variable) -> Result<Value, ExprError> {
    Ok(match variable {
        Variable::Null => Value::Null,
        Variable::Bool(b) => Value::Bool(*b),
        Variable::Number(decimal) => {
            Value::Number(Number::from_canonical(decimal.normalize().to_string()))
        }
        Variable::String(s) => Value::String(s.to_string()),
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
                .collect::<Result<_, ExprError>>()?,
        ),
        Variable::Dynamic(dynamic) => match dynamic.as_any().downcast_ref::<Temporal>() {
            Some(temporal) => Value::String(temporal.to_string()),
            None => {
                return Err(ExprError::UnsupportedResult {
                    type_name: variable.type_name().to_owned(),
                });
            }
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{from_variable, to_variable};
    use crate::{ExprError, Value};

    #[test]
    fn round_trip() -> Result<(), ExprError> {
        let value = Value::object([
            ("n", Value::Number("-12.5".parse()?)),
            ("s", Value::from("hé")),
            ("a", Value::Array(vec![Value::Null, Value::Bool(true)])),
            ("o", Value::object([("k", Value::from(1))])),
        ]);
        assert_eq!(from_variable(&to_variable(&value)?)?, value);
        Ok(())
    }
}
