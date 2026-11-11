//! Shared bounded finite-domain enumeration and expression evaluation.
use crate::table_analysis::{MAX_CONTEXTS, MAX_DOMAIN_BYTES, Unknown};
use norn_expr::schema::Schema;
use norn_expr::{EvaluationBudget, Expression, Value};
use serde_json::Value as Json;
pub(crate) const EXPRESSION_FUEL: u64 = 1_000;

#[derive(Default)]
pub(crate) struct Domain {
    pub(crate) values: Vec<Json>,
    pub(crate) bytes: usize,
}

impl Domain {
    fn push(&mut self, value: Json) -> Result<(), Unknown> {
        if self.values.len() >= MAX_CONTEXTS {
            return Err(Unknown::Limit);
        }
        let size = norn_canonical::canonical_json_of(&value)
            .map_err(|_serialization_error| Unknown::Limit)?
            .len();
        self.bytes = self.bytes.checked_add(size).ok_or(Unknown::Limit)?;
        if self.bytes > MAX_DOMAIN_BYTES {
            return Err(Unknown::Limit);
        }
        self.values.push(value);
        Ok(())
    }
}

pub(crate) fn domain(schema: &Schema) -> Result<Domain, Unknown> {
    let mut out = Domain::default();
    match schema {
        Schema::Boolean => {
            out.push(Json::Bool(false))?;
            out.push(Json::Bool(true))?;
        }
        Schema::Null => out.push(Json::Null)?,
        Schema::String(Some(values)) => {
            for value in values {
                out.push(Json::String(value.clone()))?;
            }
        }
        Schema::Nullable(inner) => {
            out = domain(inner)?;
            out.push(Json::Null)?;
        }
        Schema::Object {
            properties,
            required,
        } => {
            out.push(Json::Object(serde_json::Map::new()))?;
            for (key, schema) in properties {
                let field = domain(schema)?;
                let mut next = Domain::default();
                for object in &out.values {
                    if !required.contains(key) {
                        next.push(object.clone())?;
                    }
                    for value in &field.values {
                        let mut object = object.as_object().cloned().ok_or(Unknown::Limit)?;
                        object.insert(key.clone(), value.clone());
                        next.push(Json::Object(object))?;
                    }
                }
                out = next;
            }
        }
        Schema::Array(_)
        | Schema::String(None)
        | Schema::Integer
        | Schema::Number
        | Schema::Decimal(_)
        | Schema::Json => {
            return Err(Unknown::UnboundedDomain);
        }
    }
    Ok(out)
}

fn noncanonical(value: &Value) -> bool {
    match value {
        Value::String(text) => norn_ir::canonical::nfc(text) != *text,
        Value::Array(items) => items.iter().any(noncanonical),
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| norn_ir::canonical::nfc(key) != *key || noncanonical(value)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

pub(crate) fn expression(source: &str, context: &Value) -> Result<Value, Unknown> {
    let value = Expression::compile(source)
        .and_then(|compiled| compiled.evaluate(context, EvaluationBudget::new(EXPRESSION_FUEL)))
        .map(|result| result.value().clone())
        .map_err(|error| Unknown::Expression(error.to_string()))?;
    if noncanonical(&value) {
        return Err(Unknown::NonCanonicalIntermediate);
    }
    Ok(value)
}
