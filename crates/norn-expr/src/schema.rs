//! The closed `RuleIR` v2 schema dialect, checked against exact values.
//!
//! `norn-checker` (`validate_closed_schema`, `RuleIR` profile) admits exactly
//! this dialect: `object` with `properties`, optional `required`, and
//! `additionalProperties: false`; `array` with `items`; `string` with an
//! optional `enum`; `integer`, `number`, `boolean`, and `null`. It
//! also admits a two-member type array containing one of those non-null
//! types and `null`, preserving the same closed schema for the non-null value.
//! The evaluator re-parses it (fail closed) and checks values against it here, on
//! [`Value`]'s exact decimals.
//!
//! Shared by decision output validation and effective JSON property contracts.
//! Action inputs have a separate, equally closed profile: they additionally
//! permit nullable primitive leaves and one exact six-type JSON leaf for
//! `MappingIR`'s bounded `Type::Json` inputs.
//! Hand-written rather than a JSON Schema crate on purpose: the dialect is
//! closed and tiny, SKELETON.md admits no schema crate in `norn-decisions`,
//! and at the current fork pin zen's own (jsonschema) validation sees numbers
//! through a lossy conversion, so `integer` would accept `1.5` (upstream #530).

use std::collections::{BTreeMap, BTreeSet};

use crate::Value;
use norn_ir::JsonValue;

/// Deepest schema nesting the checker admits.
const MAX_SCHEMA_DEPTH: usize = 32;

/// One closed schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schema {
    /// A closed object record.
    Object {
        /// Allowed member schemas.
        properties: BTreeMap<String, Schema>,
        /// Required member names.
        required: BTreeSet<String>,
    },
    /// An array of values satisfying the item schema.
    Array(Box<Schema>),
    /// A string, optionally restricted to an enum.
    String(Option<BTreeSet<String>>),
    /// An exact integer.
    Integer,
    /// An exact decimal number.
    Number,
    /// Action-only exact decimal, supplied as text and decoded before execution.
    Decimal(u32),
    /// A boolean.
    Boolean,
    /// The null value.
    Null,
    /// Any bounded JSON value (the complete `RuntimeValue` domain).
    Json,
    /// A value satisfying a closed schema, or null.
    Nullable(Box<Schema>),
}

/// A schema or value defect at a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The schema or value location.
    pub path: String,
    /// Why the schema or value was refused.
    pub reason: String,
}

impl Violation {
    fn new(path: &str, reason: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            reason: reason.into(),
        }
    }
}

impl Schema {
    /// Parses a closed schema document.
    ///
    /// # Errors
    /// Returns a violation for unsupported keywords, shapes, or excessive depth.
    pub fn parse(value: &JsonValue, path: &str) -> Result<Self, Violation> {
        Self::parse_at(value, path, 0, false)
    }

    /// Parses the narrower `ActionIR` input dialect, including its bounded JSON
    /// and nullable primitive leaf schemas.
    ///
    /// Its JSON escape is a leaf, and its nullable forms are primitive leaves
    /// only. Nullable closed records and arrays remain RuleIR-only forms.
    /// Existing non-object scalar input
    /// schemas retain their existing parsing behavior, though an `ActionCall`
    /// carries an object.
    ///
    /// # Errors
    /// Returns a violation for unsupported keywords, shapes, or excessive depth.
    pub fn parse_action(value: &JsonValue, path: &str) -> Result<Self, Violation> {
        if value
            .as_object()
            .and_then(|schema| schema.get("type"))
            .is_some_and(|schema_type| schema_type.as_array().is_some())
        {
            return Err(Violation::new(
                path,
                "ActionIR type arrays are admitted only as input leaf schemas",
            ));
        }
        Self::parse_at(value, path, 0, true)
    }

    fn parse_at(
        value: &JsonValue,
        path: &str,
        depth: usize,
        action_input: bool,
    ) -> Result<Self, Violation> {
        if depth > MAX_SCHEMA_DEPTH {
            return Err(Violation::new(path, "schema nesting exceeds 32 levels"));
        }
        let schema = value
            .as_object()
            .ok_or_else(|| Violation::new(path, "a closed schema must be an object"))?;
        let type_value = schema
            .get("type")
            .ok_or_else(|| Violation::new(path, "a closed schema requires one explicit type"))?;
        if let Some(type_members) = type_value.as_array() {
            return if action_input {
                parse_action_type_union(type_members, schema, path)
            } else {
                Self::parse_nullable(type_members, schema, path, depth)
            };
        }
        let schema_type = type_value
            .as_string()
            .ok_or_else(|| Violation::new(path, "a closed schema requires one explicit type"))?;
        if action_input && schema_type == "string" && schema.contains_key("enum") {
            return Err(Violation::new(
                path,
                "ActionIR input string schemas do not admit enum",
            ));
        }
        let allowed: &[&str] = match schema_type {
            "object" => &["type", "properties", "required", "additionalProperties"],
            "array" => &["type", "items"],
            "string" => &["type", "enum"],
            "decimal" if action_input => &["type", "scale"],
            _ => &["type"],
        };
        if let Some(unknown) = schema.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(Violation::new(
                path,
                format!("`{unknown}` is not admitted in a closed `{schema_type}` schema"),
            ));
        }
        let child = depth.saturating_add(1);
        match schema_type {
            "object" => Self::parse_object(schema, path, child, action_input),
            "array" => {
                let items = schema
                    .get("items")
                    .ok_or_else(|| Violation::new(path, "a closed array schema requires items"))?;
                Ok(Self::Array(Box::new(Self::parse_at(
                    items,
                    &format!("{path}.items"),
                    child,
                    action_input,
                )?)))
            }
            "string" => match schema.get("enum") {
                None => Ok(Self::String(None)),
                Some(values) => Ok(Self::String(Some(parse_enum(values, path)?))),
            },
            "decimal" if action_input => {
                let scale = schema.get("scale").and_then(|value| match value {
                    JsonValue::Number(scale) if scale.is_finite() && scale.fract() == 0.0 && (0.0..=28.0).contains(scale) => Some(*scale as u32),
                    _ => None,
                })
                    .ok_or_else(|| Violation::new(path, "decimal scale must be an integer in 0..=28"))?;
                Ok(Self::Decimal(scale))
            }
            "integer" => Ok(Self::Integer),
            "number" => Ok(Self::Number),
            "boolean" => Ok(Self::Boolean),
            "null" => Ok(Self::Null),
            other => Err(Violation::new(
                path,
                format!("unsupported closed schema type `{other}`"),
            )),
        }
    }

    fn parse_nullable(
        members: &[JsonValue],
        schema: &BTreeMap<String, JsonValue>,
        path: &str,
        depth: usize,
    ) -> Result<Self, Violation> {
        let names: BTreeSet<&str> = members.iter().filter_map(JsonValue::as_string).collect();
        if members.len() != 2 || names.len() != 2 || !names.contains("null") {
            return Err(Violation::new(
                path,
                "a closed type array must contain one non-null type and null",
            ));
        }
        let non_null = names
            .into_iter()
            .find(|name| *name != "null")
            .ok_or_else(|| Violation::new(path, "a nullable schema needs a non-null type"))?;
        let mut inner = schema.clone();
        inner.insert("type".to_owned(), JsonValue::String(non_null.to_owned()));
        Self::parse_at(&JsonValue::Object(inner), path, depth, false)
            .map(|schema| Self::Nullable(Box::new(schema)))
    }

    fn parse_object(
        schema: &BTreeMap<String, JsonValue>,
        path: &str,
        child: usize,
        action_input: bool,
    ) -> Result<Self, Violation> {
        if schema.get("additionalProperties") != Some(&JsonValue::Bool(false)) {
            return Err(Violation::new(
                path,
                "a closed object schema requires additionalProperties=false",
            ));
        }
        let declared = schema
            .get("properties")
            .and_then(JsonValue::as_object)
            .ok_or_else(|| {
                Violation::new(path, "a closed object schema requires a properties object")
            })?;
        let mut properties = BTreeMap::new();
        for (name, property) in declared {
            properties.insert(
                name.clone(),
                Self::parse_at(
                    property,
                    &format!("{path}.properties.{name}"),
                    child,
                    action_input,
                )?,
            );
        }
        let mut required = BTreeSet::new();
        if let Some(members) = schema.get("required") {
            let members = members.as_array().ok_or_else(|| {
                Violation::new(path, "required must be an array of declared field names")
            })?;
            for member in members {
                match member.as_string() {
                    Some(name) if properties.contains_key(name) => {
                        required.insert(name.to_owned());
                    }
                    _ => {
                        return Err(Violation::new(
                            path,
                            "required must name declared properties",
                        ));
                    }
                }
            }
        }
        Ok(Self::Object {
            properties,
            required,
        })
    }

    /// The declared properties of an object schema.
    #[must_use]
    pub fn properties(&self) -> Option<&BTreeMap<String, Schema>> {
        match self {
            Self::Object { properties, .. } => Some(properties),
            Self::Array(_)
            | Self::String(_)
            | Self::Integer
            | Self::Number
            | Self::Decimal(_)
            | Self::Boolean
            | Self::Null
            | Self::Json
            | Self::Nullable(_) => None,
        }
    }

    /// The required members of an object schema.
    #[must_use]
    pub fn required(&self) -> Option<&BTreeSet<String>> {
        match self {
            Self::Object { required, .. } => Some(required),
            Self::Array(_)
            | Self::String(_)
            | Self::Integer
            | Self::Number
            | Self::Decimal(_)
            | Self::Boolean
            | Self::Null
            | Self::Json
            | Self::Nullable(_) => None,
        }
    }

    /// Decode action wire decimals without a floating-point intermediate.
    /// Other fields retain their values; the closed schema checks them afterward.
    ///
    /// # Errors
    /// Refuses non-string decimals, invalid literals and excess scale.
    pub fn decode_action_input(
        &self,
        value: norn_ir::RuntimeValue,
        path: &str,
    ) -> Result<norn_ir::RuntimeValue, Violation> {
        use norn_ir::{Decimal, RuntimeValue};
        match (self, value) {
            (Self::Decimal(scale), RuntimeValue::String(text)) => {
                let decimal = Decimal::parse(&text)
                    .map_err(|_| Violation::new(path, "invalid decimal text"))?;
                if decimal.scale() > *scale {
                    return Err(Violation::new(path, "decimal exceeds declared scale"));
                }
                Ok(RuntimeValue::Number(decimal))
            }
            (Self::Decimal(_), _) => Err(Violation::new(path, "decimal input requires a JSON string")),
            (Self::Object { properties, .. }, RuntimeValue::Object(members)) => {
                members.into_iter().map(|(name, value)| {
                    let value = match properties.get(&name) {
                        Some(schema) => schema.decode_action_input(value, &format!("{path}.{name}"))?,
                        None => value,
                    };
                    Ok((name, value))
                }).collect::<Result<_, _>>().map(RuntimeValue::Object)
            }
            (Self::Array(items), RuntimeValue::Array(values)) => values.into_iter()
                .enumerate().map(|(index, value)| items.decode_action_input(value, &format!("{path}[{index}]")))
                .collect::<Result<_, _>>().map(RuntimeValue::Array),
            (_, value) => Ok(value),
        }
    }

    /// Checks `value` against this schema.
    ///
    /// # Errors
    /// Returns the first value that violates the closed contract.
    pub fn check(&self, value: &Value, path: &str) -> Result<(), Violation> {
        match (self, value) {
            (
                Self::Object {
                    properties,
                    required,
                },
                Value::Object(members),
            ) => {
                if let Some(missing) = required.iter().find(|name| !members.contains_key(*name)) {
                    return Err(Violation::new(
                        path,
                        format!("required member `{missing}` is missing"),
                    ));
                }
                for (name, member) in members {
                    let schema = properties.get(name).ok_or_else(|| {
                        Violation::new(path, format!("member `{name}` is not declared"))
                    })?;
                    schema.check(member, &format!("{path}.{name}"))?;
                }
                Ok(())
            }
            (Self::Array(items), Value::Array(elements)) => {
                for (index, element) in elements.iter().enumerate() {
                    items.check(element, &format!("{path}[{index}]"))?;
                }
                Ok(())
            }
            (Self::String(None), Value::String(_))
            | (Self::Number, Value::Number(_))
            | (Self::Boolean, Value::Bool(_))
            | (Self::Null | Self::Nullable(_), Value::Null)
            | (Self::Json, _) => Ok(()),
            (Self::Decimal(scale), Value::Number(number)) if norn_ir::Decimal::parse(number.as_str()).is_ok_and(|decimal| decimal.scale() <= *scale) => Ok(()),
            (Self::Nullable(inner), actual) => inner.check(actual, path),
            (Self::String(Some(allowed)), Value::String(text)) => {
                if allowed.contains(text) {
                    Ok(())
                } else {
                    Err(Violation::new(
                        path,
                        format!("`{text}` is not one of the declared enum values"),
                    ))
                }
            }
            // Numbers are canonical: an integer has no fractional part.
            (Self::Integer, Value::Number(number)) if !number.as_str().contains('.') => Ok(()),
            (expected, actual) => Err(Violation::new(
                path,
                format!("expected {}, got {}", expected.name(), value_name(actual)),
            )),
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Object { .. } => "an object",
            Self::Array(_) => "an array",
            Self::String(_) => "a string",
            Self::Integer => "an integer",
            Self::Number => "a number",
            Self::Decimal(_) => "an exact decimal",
            Self::Boolean => "a boolean",
            Self::Null => "null",
            Self::Json => "a JSON value",
            Self::Nullable(_) => "a nullable value",
        }
    }
}

fn parse_action_type_union(
    members: &[JsonValue],
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
) -> Result<Schema, Violation> {
    if schema.len() != 1 || !schema.contains_key("type") {
        return Err(Violation::new(
            path,
            "an ActionIR type-array leaf permits only the type keyword",
        ));
    }
    let names: Vec<&str> = members
        .iter()
        .map(JsonValue::as_string)
        .collect::<Option<_>>()
        .ok_or_else(|| Violation::new(path, "ActionIR type arrays must contain type names"))?;
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    let json_domain: BTreeSet<&str> = ["null", "boolean", "number", "string", "array", "object"]
        .into_iter()
        .collect();
    if names.len() == 6 && unique.len() == 6 && unique == json_domain {
        return Ok(Schema::Json);
    }
    if names.len() == 2 && unique.len() == 2 && unique.contains("null") {
        let scalar = unique
            .iter()
            .copied()
            .find(|name| *name != "null")
            .filter(|name| matches!(*name, "string" | "integer" | "number" | "boolean"));
        if let Some(scalar) = scalar {
            let inner = match scalar {
                "string" => Schema::String(None),
                "integer" => Schema::Integer,
                "number" => Schema::Number,
                "boolean" => Schema::Boolean,
                _ => {
                    return Err(Violation::new(
                        &format!("{path}.type"),
                        "ActionIR type array must contain a nullable primitive scalar",
                    ));
                }
            };
            return Ok(Schema::Nullable(Box::new(inner)));
        }
    }
    Err(Violation::new(
        &format!("{path}.type"),
        "ActionIR type array must be a nullable scalar or the complete JSON value domain",
    ))
}

fn parse_enum(values: &JsonValue, path: &str) -> Result<BTreeSet<String>, Violation> {
    let values = values
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or_else(|| Violation::new(path, "enum must be a non-empty array of strings"))?;
    let mut allowed = BTreeSet::new();
    for value in values {
        let text = value
            .as_string()
            .ok_or_else(|| Violation::new(path, "enum values must be strings"))?;
        if !allowed.insert(text.to_owned()) {
            return Err(Violation::new(
                path,
                format!("enum value `{text}` is duplicated"),
            ));
        }
    }
    Ok(allowed)
}

const fn value_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::Schema;
    use crate::{Number, Value};
    use norn_ir::JsonValue;

    fn obj(entries: Vec<(&str, JsonValue)>) -> JsonValue {
        JsonValue::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    fn ty(name: &str) -> JsonValue {
        obj(vec![("type", JsonValue::String(name.to_owned()))])
    }

    fn type_array(names: &[&str]) -> JsonValue {
        obj(vec![(
            "type",
            JsonValue::Array(
                names
                    .iter()
                    .map(|name| JsonValue::String((*name).to_owned()))
                    .collect(),
            ),
        )])
    }

    fn action_record(json_type: JsonValue, nullable_type: JsonValue) -> JsonValue {
        obj(vec![
            ("type", JsonValue::String("object".to_owned())),
            (
                "properties",
                obj(vec![("json", json_type), ("email", nullable_type)]),
            ),
            (
                "required",
                JsonValue::Array(vec![
                    JsonValue::String("json".to_owned()),
                    JsonValue::String("email".to_owned()),
                ]),
            ),
            ("additionalProperties", JsonValue::Bool(false)),
        ])
    }

    fn record() -> Schema {
        Schema::parse(
            &obj(vec![
                ("type", JsonValue::String("object".to_owned())),
                (
                    "properties",
                    obj(vec![
                        ("n", ty("integer")),
                        ("x", ty("number")),
                        (
                            "op",
                            obj(vec![
                                ("type", JsonValue::String("string".to_owned())),
                                (
                                    "enum",
                                    JsonValue::Array(vec![JsonValue::String("create".to_owned())]),
                                ),
                            ]),
                        ),
                    ]),
                ),
                (
                    "required",
                    JsonValue::Array(vec![JsonValue::String("n".to_owned())]),
                ),
                ("additionalProperties", JsonValue::Bool(false)),
            ]),
            "schema",
        )
        .unwrap()
    }

    fn num(text: &str) -> Value {
        Value::Number(text.parse::<Number>().unwrap())
    }

    #[test]
    fn integer_is_exact() {
        let schema = record();
        assert!(
            schema
                .check(&Value::object([("n", num("2.0"))]), "$")
                .is_ok()
        );
        let err = schema
            .check(&Value::object([("n", num("1.5"))]), "$")
            .unwrap_err();
        assert_eq!(err.path, "$.n");
    }

    #[test]
    fn closed_members_enum_and_required() {
        let schema = record();
        assert!(
            schema
                .check(
                    &Value::object([("n", num("1")), ("extra", Value::Null)]),
                    "$"
                )
                .is_err()
        );
        assert!(
            schema
                .check(&Value::object([("x", num("1"))]), "$")
                .is_err()
        );
        assert!(
            schema
                .check(
                    &Value::object([("n", num("1")), ("op", Value::from("delete"))]),
                    "$"
                )
                .is_err()
        );
        assert!(
            schema
                .check(
                    &Value::object([("n", num("1")), ("op", Value::from("create"))]),
                    "$"
                )
                .is_ok()
        );
    }

    #[test]
    fn open_schemas_are_refused() {
        let open = obj(vec![
            ("type", JsonValue::String("object".to_owned())),
            ("properties", obj(vec![])),
        ]);
        assert!(Schema::parse(&open, "s").is_err());
        let unknown = obj(vec![
            ("type", JsonValue::String("number".to_owned())),
            ("minimum", JsonValue::Number(0.0)),
        ]);
        assert!(Schema::parse(&unknown, "s").is_err());
    }

    #[test]
    fn nullable_closed_schemas_validate_null_and_the_complete_non_null_shape() {
        let scalar = Schema::parse(&type_array(&["integer", "null"]), "s").unwrap();
        assert!(scalar.check(&Value::Null, "s").is_ok());
        assert!(scalar.check(&num("42"), "s").is_ok());
        assert!(scalar.check(&num("1.5"), "s").is_err());
        assert!(scalar.check(&Value::from("42"), "s").is_err());

        let object = obj(vec![
            (
                "type",
                JsonValue::Array(vec![
                    JsonValue::String("object".into()),
                    JsonValue::String("null".into()),
                ]),
            ),
            ("properties", obj(vec![("name", ty("string"))])),
            (
                "required",
                JsonValue::Array(vec![JsonValue::String("name".into())]),
            ),
            ("additionalProperties", JsonValue::Bool(false)),
        ]);
        let schema = Schema::parse(&object, "s").unwrap();
        assert!(schema.check(&Value::Null, "s").is_ok());
        assert!(
            schema
                .check(&Value::object([("name", Value::from("Size"))]), "s")
                .is_ok()
        );
        assert!(
            schema
                .check(&Value::object([("name", num("2"))]), "s")
                .is_err()
        );
        assert!(
            schema
                .check(&Value::object([("other", Value::from("Size"))]), "s")
                .is_err()
        );

        let array = obj(vec![
            (
                "type",
                JsonValue::Array(vec![
                    JsonValue::String("array".into()),
                    JsonValue::String("null".into()),
                ]),
            ),
            ("items", type_array(&["boolean", "null"])),
        ]);
        let schema = Schema::parse(&array, "s").unwrap();
        assert!(schema.check(&Value::Null, "s").is_ok());
        assert!(
            schema
                .check(&Value::Array(vec![Value::Bool(true), Value::Null]), "s")
                .is_ok()
        );
        assert!(schema.check(&Value::Array(vec![num("2")]), "s").is_err());
        assert!(Schema::parse_action(&action_record(object, ty("string")), "s").is_err());
    }

    #[test]
    fn nullable_closed_schemas_reject_arbitrary_unions_and_open_shapes() {
        for names in [
            vec!["string", "integer"],
            vec!["string", "null", "number"],
            vec!["null", "null"],
            vec!["string", "string"],
            vec!["unknown", "null"],
        ] {
            assert!(Schema::parse(&type_array(&names), "s").is_err());
        }
        let open = obj(vec![
            (
                "type",
                JsonValue::Array(vec![
                    JsonValue::String("object".into()),
                    JsonValue::String("null".into()),
                ]),
            ),
            ("properties", obj(vec![])),
            ("additionalProperties", JsonValue::Bool(true)),
        ]);
        assert!(Schema::parse(&open, "s").is_err());
        assert!(Schema::parse(&type_array(&["array", "null"]), "s").is_err());
    }

    #[test]
    fn action_profile_accepts_bounded_json_and_nullable_scalars() {
        let action = action_record(
            type_array(&["null", "boolean", "number", "string", "array", "object"]),
            type_array(&["string", "null"]),
        );
        assert!(Schema::parse(&action, "$input").is_err());
        let schema = Schema::parse_action(&action, "$input").unwrap();
        assert!(
            schema
                .check(
                    &Value::object([
                        (
                            "json",
                            Value::object([("nested", Value::Array(vec![Value::from(true)]))]),
                        ),
                        ("email", Value::Null),
                    ]),
                    "$input",
                )
                .is_ok()
        );
        assert!(
            schema
                .check(
                    &Value::object([
                        ("json", Value::Null),
                        ("email", Value::from("person@example.test")),
                    ]),
                    "$input",
                )
                .is_ok()
        );
        assert!(
            schema
                .check(
                    &Value::object([("json", num("2.5")), ("email", Value::from(42)),]),
                    "$input",
                )
                .is_err()
        );
        assert!(
            schema
                .check(
                    &Value::object([
                        ("json", Value::Null),
                        ("email", Value::Null),
                        ("unmapped", Value::from("blocked")),
                    ]),
                    "$input",
                )
                .is_err()
        );
    }

    #[test]
    fn action_profile_rejects_open_or_arbitrary_union_schemas() {
        for leaf in [
            type_array(&["string", "integer"]),
            type_array(&["string", "null", "number"]),
            type_array(&[
                "null", "boolean", "number", "string", "array", "object", "object",
            ]),
            obj(vec![
                (
                    "type",
                    JsonValue::Array(vec![JsonValue::String("string".to_owned())]),
                ),
                ("minimum", JsonValue::Number(0.0)),
            ]),
        ] {
            let action = action_record(leaf, type_array(&["string", "null"]));
            assert!(Schema::parse_action(&action, "$input").is_err());
        }
        assert!(Schema::parse_action(&ty("string"), "$input").is_ok());
        assert!(
            Schema::parse_action(
                &type_array(&["null", "boolean", "number", "string", "array", "object"]),
                "$input"
            )
            .is_err()
        );
    }
}
