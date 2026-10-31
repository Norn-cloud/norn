//! Exact policy wire values. General definition JSON keeps its binary64 contract.
use std::collections::BTreeMap;

use norn_canonical::{JsonNumber, JsonNumberKind, JsonTree};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::JsonValue;

/// Lenient policy storage, retaining signed and unsigned integer tokens exactly.
/// Strict policy types still decide which values are admissible.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum PolicyValue {
    /// JSON null.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed integer, preserved without a floating-point conversion.
    Signed(i64),
    /// Nonnegative integer, including values the strict Long type must refuse.
    Unsigned(u64),
    /// Floating-point source values remain distinct from integer tokens.
    Float(f64),
    /// String.
    String(String),
    /// Ordered array.
    Array(Vec<Self>),
    /// Object with ordered keys.
    Object(BTreeMap<String, Self>),
}

impl<'de> Deserialize<'de> for PolicyValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        norn_canonical::deserialize_json_tree(deserializer)
    }
}

impl JsonTree for PolicyValue {
    fn null() -> Self {
        Self::Null
    }
    fn boolean(value: bool) -> Self {
        Self::Bool(value)
    }
    fn number(value: JsonNumber) -> Self {
        match value.kind() {
            JsonNumberKind::NegativeInteger(value) => Self::Signed(value),
            JsonNumberKind::NonNegativeInteger(value) => Self::Unsigned(value),
            JsonNumberKind::Float => Self::Float(value.as_f64()),
        }
    }
    fn string(value: String) -> Self {
        Self::String(value)
    }
    fn array(values: Vec<Self>) -> Self {
        Self::Array(values)
    }
    fn object(entries: Vec<(String, Self)>) -> Self {
        Self::Object(entries.into_iter().collect())
    }
}

impl JsonSchema for PolicyValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        JsonValue::schema_name()
    }
    fn schema_id() -> std::borrow::Cow<'static, str> {
        JsonValue::schema_id()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        JsonValue::json_schema(generator)
    }
}

impl PolicyValue {
    /// Decode a strict policy type while retaining every integer token.
    ///
    /// # Errors
    /// The stored value does not satisfy the requested typed contract.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::to_value(self).and_then(serde_json::from_value)
    }

    /// A binary64 projection for existing structural diagnostics only.
    /// Policy interpretation and serialization must use the exact storage.
    #[must_use]
    pub fn structural_projection(&self) -> JsonValue {
        match self {
            Self::Null => JsonValue::Null,
            Self::Bool(value) => JsonValue::Bool(*value),
            Self::Signed(value) => integer_projection(&value.to_string()),
            Self::Unsigned(value) => integer_projection(&value.to_string()),
            Self::Float(value) => JsonValue::Number(*value),
            Self::String(value) => JsonValue::String(value.clone()),
            Self::Array(values) => {
                JsonValue::Array(values.iter().map(Self::structural_projection).collect())
            }
            Self::Object(values) => JsonValue::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), value.structural_projection()))
                    .collect(),
            ),
        }
    }
}

// This conversion is deliberately lossy and has no policy-admission authority.
fn integer_projection(text: &str) -> JsonValue {
    JsonValue::Number(JsonNumber::parse_literal(text).map_or(0.0, JsonNumber::as_f64))
}

impl From<JsonValue> for PolicyValue {
    fn from(value: JsonValue) -> Self {
        match value {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(value) => Self::Bool(value),
            JsonValue::Number(value) => Self::Float(value),
            JsonValue::String(value) => Self::String(value),
            JsonValue::Array(values) => Self::Array(values.into_iter().map(Self::from).collect()),
            JsonValue::Object(values) => Self::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value)))
                    .collect(),
            ),
        }
    }
}
