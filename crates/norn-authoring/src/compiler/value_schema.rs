//! Closed JSON value schemas (`tools/norn-ts/src/builder/schema.ts`).
//!
//! Structural merging, assignability, and expansion budgets live in
//! [`super::value_schema_ops`] and are re-exported here.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::value_types::DecimalType;

pub(crate) use super::value_schema_ops::{
    BudgetExceeded, ExpansionBudget, ExpansionLimits, assignable, merge_schemas,
};

/// Display-only labels admitted on `ActionIR` input schemas (`D66`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaLabels {
    /// Optional human-readable field title.
    pub title: Option<String>,
    /// Optional human-readable field description.
    pub description: Option<String>,
}

/// One wizard step over an `ActionIR` input (`x-norn.groups`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputGroup {
    /// Group identifier.
    pub id: String,
    /// Human-readable group title.
    pub title: String,
    /// RFC 6901 field pointers belonging to this group.
    pub fields: Vec<String>,
}

/// Closed object schema (`type: "object", additionalProperties: false`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectSchema {
    /// Field schemas keyed by property name.
    pub properties: BTreeMap<String, ValueSchema>,
    /// Required property names in declaration order (omitted on wire when empty).
    pub required: Vec<String>,
    /// Optional display labels.
    pub labels: SchemaLabels,
    /// Optional `ActionIR` input wizard groups (`x-norn.groups`).
    pub groups: Vec<InputGroup>,
}

impl ObjectSchema {
    /// Builds a closed record schema where every field is required unless in `optional`.
    #[must_use]
    pub fn record(properties: BTreeMap<String, ValueSchema>, optional: &[String]) -> Self {
        let required: Vec<String> = properties
            .keys()
            .filter(|name| !optional.iter().any(|opt| opt == *name))
            .cloned()
            .collect();
        Self {
            properties,
            required,
            labels: SchemaLabels::default(),
            groups: Vec::new(),
        }
    }

    /// Builds a closed record schema preserving explicit required field order.
    #[must_use]
    pub fn record_ordered(ordered_fields: Vec<(String, ValueSchema)>, optional: &[String]) -> Self {
        let mut required = Vec::new();
        let mut properties = BTreeMap::new();
        for (name, schema) in ordered_fields {
            if !optional.iter().any(|opt| opt == &name) {
                required.push(name.clone());
            }
            properties.insert(name, schema);
        }
        Self {
            properties,
            required,
            labels: SchemaLabels::default(),
            groups: Vec::new(),
        }
    }

    /// Builds an empty closed record schema (`{}`).
    #[must_use]
    pub fn empty() -> Self {
        Self::record(BTreeMap::new(), &[])
    }

    /// Iterates property names in deterministic order.
    #[must_use]
    pub fn ordered_keys(&self) -> impl Iterator<Item = &str> {
        self.properties.keys().map(String::as_str)
    }

    /// Merges D66 per-field display labels onto declared properties.
    pub fn apply_property_labels(&mut self, labels: BTreeMap<String, SchemaLabels>) {
        for (field, extra) in labels {
            if let Some(existing) = self.properties.remove(&field) {
                self.properties.insert(field, existing.with_labels(extra));
            }
        }
    }

    /// Serializes this object schema to canonical JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        write_object_fields(self, &mut map, false);
        Value::Object(map)
    }
}

/// Closed value schema emitted into `RuleIR`, `ActionIR`, and Function contracts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueSchema {
    /// Plain or string-enum schema (`type: "string"`).
    String {
        /// Optional closed set of allowed string values.
        enum_values: Option<Vec<String>>,
        /// Optional display labels.
        labels: SchemaLabels,
    },
    /// Exact integer schema (`type: "integer"`).
    Integer {
        /// Optional display labels.
        labels: SchemaLabels,
    },
    /// Numeric or exact-decimal schema (`type: "number"`).
    Number {
        /// Optional exact decimal contract (`nornType`).
        norn_type: Option<Box<DecimalType>>,
        /// Optional display labels.
        labels: SchemaLabels,
    },
    /// Boolean schema (`type: "boolean"`).
    Boolean {
        /// Optional display labels.
        labels: SchemaLabels,
    },
    /// Null-only schema (`type: "null"`).
    Null {
        /// Optional display labels.
        labels: SchemaLabels,
    },
    /// Homogeneous array schema (`type: "array"`).
    Array {
        /// Element schema.
        items: Box<ValueSchema>,
        /// Optional display labels.
        labels: SchemaLabels,
    },
    /// Closed object schema (`type: "object"`).
    Object(Box<ObjectSchema>),
    /// Nullable closed schema (`type: [<inner>, "null"]`).
    Nullable(Box<ValueSchema>),
    /// Unrestricted bounded JSON value union (`type: ["null", "boolean", "number", "string", "array", "object"]`).
    JsonValue {
        /// Optional display labels.
        labels: SchemaLabels,
    },
}

impl ValueSchema {
    /// Plain open string schema.
    #[must_use]
    pub fn text() -> Self {
        Self::String {
            enum_values: None,
            labels: SchemaLabels::default(),
        }
    }

    /// Closed string enumeration schema.
    #[must_use]
    pub fn text_enum(values: Vec<String>) -> Self {
        Self::String {
            enum_values: Some(values),
            labels: SchemaLabels::default(),
        }
    }

    /// Integer schema.
    #[must_use]
    pub fn integer() -> Self {
        Self::Integer {
            labels: SchemaLabels::default(),
        }
    }

    /// Numeric schema.
    #[must_use]
    pub fn number() -> Self {
        Self::Number {
            norn_type: None,
            labels: SchemaLabels::default(),
        }
    }

    /// Exact decimal numeric schema (`type: "number", nornType: ...`).
    #[must_use]
    pub fn exact_decimal(dec: DecimalType) -> Self {
        Self::Number {
            norn_type: Some(Box::new(dec)),
            labels: SchemaLabels::default(),
        }
    }

    /// Boolean schema.
    #[must_use]
    pub fn boolean() -> Self {
        Self::Boolean {
            labels: SchemaLabels::default(),
        }
    }

    /// Null schema.
    #[must_use]
    pub fn null_value() -> Self {
        Self::Null {
            labels: SchemaLabels::default(),
        }
    }

    /// Unrestricted bounded JSON value union.
    #[must_use]
    pub fn json_value() -> Self {
        Self::JsonValue {
            labels: SchemaLabels::default(),
        }
    }

    /// Array schema.
    #[must_use]
    pub fn array(items: Self) -> Self {
        Self::Array {
            items: Box::new(items),
            labels: SchemaLabels::default(),
        }
    }

    /// Object schema.
    #[must_use]
    pub fn object(obj: ObjectSchema) -> Self {
        Self::Object(Box::new(obj))
    }

    /// Wraps `inner` as a nullable schema unless it is already `Null`, `Nullable`, or `JsonValue`.
    #[must_use]
    pub fn nullable(inner: Self) -> Self {
        inner.into_nullable()
    }

    /// Wraps `self` as a nullable schema unless it is already `Null`, `Nullable`, or `JsonValue`.
    #[must_use]
    pub fn into_nullable(self) -> Self {
        match self {
            Self::Null { .. } | Self::Nullable(_) | Self::JsonValue { .. } => self,
            other => Self::Nullable(Box::new(other)),
        }
    }

    /// Strips the null wrapper from a two-type nullable schema.
    #[must_use]
    pub fn non_null(&self) -> &Self {
        match self {
            Self::Nullable(inner) => inner.as_ref(),
            other => other,
        }
    }

    /// Returns `true` for scalar schemas (everything except non-null `Object` and `Array`).
    #[must_use]
    pub const fn is_scalar(&self) -> bool {
        !matches!(self, Self::Object(_) | Self::Array { .. })
    }

    /// Returns `true` for `Integer` or `Number` (non-nullable).
    #[must_use]
    pub const fn is_numeric(&self) -> bool {
        matches!(self, Self::Integer { .. } | Self::Number { .. })
    }

    /// Returns the wire type name (or `"null"` / compound display name for diagnostics).
    #[must_use]
    pub fn type_label(&self) -> String {
        match self {
            Self::String { .. } => "string".to_owned(),
            Self::Integer { .. } => "integer".to_owned(),
            Self::Number { .. } => "number".to_owned(),
            Self::Boolean { .. } => "boolean".to_owned(),
            Self::Null { .. } => "null".to_owned(),
            Self::Array { .. } => "array".to_owned(),
            Self::Object(_) => "object".to_owned(),
            Self::Nullable(inner) => format!("{},null", inner.type_label()),
            Self::JsonValue { .. } => "null,boolean,number,string,array,object".to_owned(),
        }
    }

    /// Returns `true` if this schema or any nested field/item carries an exact `nornType` decimal.
    #[must_use]
    pub fn has_exact(&self) -> bool {
        match self {
            Self::Number { norn_type, .. } => norn_type.is_some(),
            Self::Array { items, .. } => items.has_exact(),
            Self::Object(obj) => obj.properties.values().any(Self::has_exact),
            Self::Nullable(inner) => inner.has_exact(),
            Self::String { .. }
            | Self::Integer { .. }
            | Self::Boolean { .. }
            | Self::Null { .. }
            | Self::JsonValue { .. } => false,
        }
    }

    /// Computes tree height matching `SchemaBudget::height` in `types.ts`.
    #[must_use]
    pub fn height(&self) -> usize {
        match self {
            Self::Object(obj) => obj
                .properties
                .values()
                .map(|child| child.height().saturating_add(1))
                .max()
                .unwrap_or(0),
            Self::Array { items, .. } => items.height().saturating_add(1),
            Self::Nullable(inner) => inner.height(),
            Self::String { .. }
            | Self::Integer { .. }
            | Self::Number { .. }
            | Self::Boolean { .. }
            | Self::Null { .. }
            | Self::JsonValue { .. } => 0,
        }
    }

    /// Attaches D66 display labels (`title`, `description`) onto this schema.
    #[must_use]
    pub fn with_labels(mut self, extra: SchemaLabels) -> Self {
        let target = match &mut self {
            Self::String { labels, .. }
            | Self::Integer { labels }
            | Self::Number { labels, .. }
            | Self::Boolean { labels }
            | Self::Null { labels }
            | Self::Array { labels, .. }
            | Self::JsonValue { labels } => labels,
            Self::Object(obj) => &mut obj.labels,
            Self::Nullable(inner) => {
                let updated = (**inner).clone().with_labels(extra);
                *inner = Box::new(updated);
                return self;
            }
        };
        if extra.title.is_some() {
            target.title = extra.title;
        }
        if extra.description.is_some() {
            target.description = extra.description;
        }
        self
    }

    /// Serializes this schema into canonical JSON matching `builder/schema.ts`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        self.write_fields(&mut map, false);
        Value::Object(map)
    }

    fn write_fields(&self, map: &mut Map<String, Value>, nullable_wrap: bool) {
        match self {
            Self::String {
                enum_values,
                labels,
            } => {
                map.insert("type".to_owned(), type_field_value("string", nullable_wrap));
                if let Some(vals) = enum_values {
                    map.insert(
                        "enum".to_owned(),
                        Value::Array(vals.iter().cloned().map(Value::String).collect()),
                    );
                }
                insert_labels(map, labels);
            }
            Self::Integer { labels } => {
                map.insert(
                    "type".to_owned(),
                    type_field_value("integer", nullable_wrap),
                );
                insert_labels(map, labels);
            }
            Self::Number { norn_type, labels } => {
                map.insert("type".to_owned(), type_field_value("number", nullable_wrap));
                if let Some(nt) = norn_type {
                    map.insert("nornType".to_owned(), nt.to_json());
                }
                insert_labels(map, labels);
            }
            Self::Boolean { labels } => {
                map.insert(
                    "type".to_owned(),
                    type_field_value("boolean", nullable_wrap),
                );
                insert_labels(map, labels);
            }
            Self::Null { labels } => {
                map.insert("type".to_owned(), Value::String("null".to_owned()));
                insert_labels(map, labels);
            }
            Self::Array { items, labels } => {
                map.insert("type".to_owned(), type_field_value("array", nullable_wrap));
                map.insert("items".to_owned(), items.to_json());
                insert_labels(map, labels);
            }
            Self::Object(obj) => write_object_fields(obj, map, nullable_wrap),
            Self::Nullable(inner) => inner.write_fields(map, true),
            Self::JsonValue { labels } => {
                map.insert(
                    "type".to_owned(),
                    Value::Array(
                        ["null", "boolean", "number", "string", "array", "object"]
                            .into_iter()
                            .map(|s| Value::String(s.to_owned()))
                            .collect(),
                    ),
                );
                insert_labels(map, labels);
            }
        }
    }
}

fn type_field_value(name: &str, nullable_wrap: bool) -> Value {
    if nullable_wrap {
        Value::Array(vec![
            Value::String(name.to_owned()),
            Value::String("null".to_owned()),
        ])
    } else {
        Value::String(name.to_owned())
    }
}

fn write_object_fields(obj: &ObjectSchema, map: &mut Map<String, Value>, nullable_wrap: bool) {
    map.insert("type".to_owned(), type_field_value("object", nullable_wrap));
    let mut props = Map::new();
    for (name, schema) in &obj.properties {
        props.insert(name.clone(), schema.to_json());
    }
    map.insert("properties".to_owned(), Value::Object(props));
    if !obj.required.is_empty() {
        map.insert(
            "required".to_owned(),
            Value::Array(obj.required.iter().cloned().map(Value::String).collect()),
        );
    }
    map.insert("additionalProperties".to_owned(), Value::Bool(false));
    if !obj.groups.is_empty() {
        let groups = obj
            .groups
            .iter()
            .map(|g| {
                let mut gm = Map::new();
                gm.insert("id".to_owned(), Value::String(g.id.clone()));
                gm.insert("title".to_owned(), Value::String(g.title.clone()));
                gm.insert(
                    "fields".to_owned(),
                    Value::Array(g.fields.iter().cloned().map(Value::String).collect()),
                );
                Value::Object(gm)
            })
            .collect();
        let mut x_norn = Map::new();
        x_norn.insert("groups".to_owned(), Value::Array(groups));
        map.insert("x-norn".to_owned(), Value::Object(x_norn));
    }
    insert_labels(map, &obj.labels);
}

fn insert_labels(map: &mut Map<String, Value>, labels: &SchemaLabels) {
    if let Some(title) = &labels.title {
        map.insert("title".to_owned(), Value::String(title.clone()));
    }
    if let Some(desc) = &labels.description {
        map.insert("description".to_owned(), Value::String(desc.clone()));
    }
}

/// Returns the schema of a JSON scalar literal (`literalSchema` in `schema.ts`).
#[must_use]
pub(crate) fn literal_schema(value: &Value) -> Option<ValueSchema> {
    match value {
        Value::Null => Some(ValueSchema::null_value()),
        Value::Bool(_) => Some(ValueSchema::boolean()),
        Value::String(_) => Some(ValueSchema::text()),
        Value::Number(num) => {
            if num.is_i64() || num.is_u64() {
                Some(ValueSchema::integer())
            } else if num
                .as_f64()
                .is_some_and(|f| f.is_finite() && f.fract().abs().to_bits() == 0)
            {
                Some(ValueSchema::integer())
            } else {
                Some(ValueSchema::number())
            }
        }
        Value::Array(_) | Value::Object(_) => None,
    }
}
