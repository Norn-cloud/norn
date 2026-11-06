//! Typed ports for gateway tools (#772, lane CONNECT).
//!
//! A tool gateway describes each tool's input and output with JSON Schema.
//! [`ports_from_schema`] lowers the subset of JSON Schema that has one Norn
//! meaning into a [`PortType`] and refuses everything else by path, so a tool
//! node never carries a type the checker cannot check. [`edge_refusal`]
//! decides whether a value of one port type may flow into another, the same
//! check the browser checker and the server run on every edge.
//!
//! The conversion is pure and sans-IO: it reads only the schema value.
//! References resolve to `#/$defs/…` or `#/definitions/…` of the same
//! document; recursive references are refused.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value, json};

/// How deeply schemas and `$ref`s may nest before conversion refuses.
pub const MAX_DEPTH: usize = 32;

/// A Norn port type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortType {
    /// A UTF-8 string.
    String,
    /// One of a closed set of strings.
    Enum(Vec<String>),
    /// An integer.
    Integer,
    /// A JSON number (not an exact decimal).
    Number,
    /// A boolean.
    Boolean,
    /// An RFC 3339 full date.
    Date,
    /// An RFC 3339 date-time.
    Datetime,
    /// A list of one item type.
    List(Box<PortType>),
    /// Named fields; `open` when other fields may also be present.
    Record {
        /// The declared fields by name.
        fields: BTreeMap<String, Field>,
        /// `additionalProperties` was not `false`.
        open: bool,
    },
    /// String keys to one value type.
    Map(Box<PortType>),
    /// The inner type or `null`.
    Nullable(Box<PortType>),
}

/// One field of a [`PortType::Record`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The field's type.
    pub port: PortType,
    /// Listed in `required`.
    pub required: bool,
}

/// Why a schema has no port type, at a JSON Pointer into the schema's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// JSON Pointer of the offending subschema (`""` for the root).
    pub path: String,
    /// What is unsupported.
    pub reason: Reason,
}

/// The kinds of JSON Schema a port type refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// No `type`, `enum`, `const` or `$ref`: any value is allowed.
    Untyped,
    /// `anyOf` / `oneOf` / a type list with more than one non-null member.
    Union,
    /// `allOf`, `not`, `if`, `patternProperties` or another combinator.
    Combinator(String),
    /// A `$ref` outside this document's `$defs`, or one that does not resolve.
    Reference(String),
    /// A `$ref` that reaches itself.
    Recursive(String),
    /// An `enum` or `const` with a non-string member.
    NonStringEnum,
    /// A type name JSON Schema does not define, or `null` alone.
    UnknownType(String),
    /// An `array` without a single `items` schema.
    UntypedItems,
    /// Nesting beyond [`MAX_DEPTH`].
    TooDeep,
    /// The schema is not an object or boolean.
    NotASchema,
}

impl Reason {
    /// The stable refusal code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Untyped => "tool_port.untyped",
            Self::Union => "tool_port.union",
            Self::Combinator(_) => "tool_port.combinator",
            Self::Reference(_) => "tool_port.reference",
            Self::Recursive(_) => "tool_port.recursive",
            Self::NonStringEnum => "tool_port.non_string_enum",
            Self::UnknownType(_) => "tool_port.unknown_type",
            Self::UntypedItems => "tool_port.untyped_items",
            Self::TooDeep => "tool_port.too_deep",
            Self::NotASchema => "tool_port.not_a_schema",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untyped => f.write_str("the schema allows any value; a port needs a type"),
            Self::Union => f.write_str("a union of several types has no single port type"),
            Self::Combinator(keyword) => write!(f, "`{keyword}` is not supported in a port type"),
            Self::Reference(target) => write!(f, "`$ref` {target} does not resolve in this schema"),
            Self::Recursive(target) => write!(f, "`$ref` {target} is recursive"),
            Self::NonStringEnum => f.write_str("only string enums are supported"),
            Self::UnknownType(name) => write!(f, "type `{name}` has no port type"),
            Self::UntypedItems => f.write_str("an array needs one `items` schema"),
            Self::TooDeep => write!(f, "nested deeper than {MAX_DEPTH}"),
            Self::NotASchema => f.write_str("not a JSON Schema object"),
        }
    }
}

const COMBINATORS: [&str; 7] = [
    "allOf",
    "not",
    "if",
    "patternProperties",
    "dependentSchemas",
    "prefixItems",
    "unevaluatedProperties",
];

/// Lower a JSON Schema to a port type.
///
/// # Errors
///
/// [`Unsupported`] names the first subschema, by JSON Pointer, that has no
/// port type.
pub fn ports_from_schema(schema: &Value) -> Result<PortType, Unsupported> {
    Lowering {
        root: schema,
        stack: Vec::new(),
    }
    .lower(schema, "", 0)
}

struct Lowering<'a> {
    root: &'a Value,
    stack: Vec<String>,
}

fn refuse<T>(path: &str, reason: Reason) -> Result<T, Unsupported> {
    Err(Unsupported {
        path: path.to_owned(),
        reason,
    })
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn string_set(values: &[Value], path: &str) -> Result<Vec<String>, Unsupported> {
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        let Some(text) = value.as_str() else {
            return refuse(path, Reason::NonStringEnum);
        };
        out.push(text.to_owned());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

impl Lowering<'_> {
    fn lower(&mut self, schema: &Value, path: &str, depth: usize) -> Result<PortType, Unsupported> {
        if depth > MAX_DEPTH {
            return refuse(path, Reason::TooDeep);
        }
        let object = match schema {
            Value::Object(object) => object,
            Value::Bool(_) => return refuse(path, Reason::Untyped),
            Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) => {
                return refuse(path, Reason::NotASchema);
            }
        };
        if let Some(keyword) = COMBINATORS.iter().find(|k| object.contains_key(**k)) {
            return refuse(path, Reason::Combinator((*keyword).to_owned()));
        }
        if let Some(target) = object.get("$ref") {
            return self.reference(target, path, depth);
        }
        for keyword in ["anyOf", "oneOf"] {
            if let Some(Value::Array(members)) = object.get(keyword) {
                return self.union(members, &format!("{path}/{keyword}"), depth);
            }
        }
        if let Some(constant) = object.get("const") {
            return Ok(PortType::Enum(string_set(
                std::slice::from_ref(constant),
                path,
            )?));
        }
        if let Some(Value::Array(values)) = object.get("enum") {
            let nullable = values.iter().any(Value::is_null);
            let strings: Vec<Value> = values.iter().filter(|v| !v.is_null()).cloned().collect();
            let port = PortType::Enum(string_set(&strings, path)?);
            return Ok(if nullable {
                PortType::Nullable(Box::new(port))
            } else {
                port
            });
        }
        match object.get("type") {
            None => refuse(path, Reason::Untyped),
            Some(Value::String(name)) => self.named(name, object, path, depth),
            Some(Value::Array(names)) => {
                let nullable = names.iter().any(|n| n == "null");
                let rest: Vec<&Value> = names.iter().filter(|n| *n != "null").collect();
                match rest.as_slice() {
                    [Value::String(name)] => {
                        let port = self.named(name, object, path, depth)?;
                        Ok(if nullable {
                            PortType::Nullable(Box::new(port))
                        } else {
                            port
                        })
                    }
                    [] => refuse(path, Reason::UnknownType("null".to_owned())),
                    _ => refuse(path, Reason::Union),
                }
            }
            Some(_) => refuse(path, Reason::NotASchema),
        }
    }

    fn reference(
        &mut self,
        target: &Value,
        path: &str,
        depth: usize,
    ) -> Result<PortType, Unsupported> {
        let Some(target) = target.as_str() else {
            return refuse(path, Reason::NotASchema);
        };
        let local = ["#/$defs/", "#/definitions/"]
            .iter()
            .any(|p| target.starts_with(p));
        let resolved = target
            .strip_prefix('#')
            .filter(|_| local)
            .and_then(|pointer| self.root.pointer(pointer));
        let Some(resolved) = resolved else {
            return refuse(path, Reason::Reference(target.to_owned()));
        };
        if self.stack.iter().any(|seen| seen == target) {
            return refuse(path, Reason::Recursive(target.to_owned()));
        }
        self.stack.push(target.to_owned());
        let port = self.lower(
            resolved,
            target.trim_start_matches('#'),
            depth.saturating_add(1),
        );
        self.stack.pop();
        port
    }

    fn union(
        &mut self,
        members: &[Value],
        path: &str,
        depth: usize,
    ) -> Result<PortType, Unsupported> {
        let is_null = |m: &Value| m.get("type").is_some_and(|t| t == "null");
        let nullable = members.iter().any(is_null);
        let rest: Vec<(usize, &Value)> = members
            .iter()
            .enumerate()
            .filter(|(_, m)| !is_null(m))
            .collect();
        let [(index, member)] = rest.as_slice() else {
            return refuse(path, Reason::Union);
        };
        let port = self.lower(member, &format!("{path}/{index}"), depth.saturating_add(1))?;
        Ok(if nullable {
            PortType::Nullable(Box::new(port))
        } else {
            port
        })
    }

    fn named(
        &mut self,
        name: &str,
        object: &Map<String, Value>,
        path: &str,
        depth: usize,
    ) -> Result<PortType, Unsupported> {
        Ok(match name {
            "string" => match object.get("format").and_then(Value::as_str) {
                Some("date") => PortType::Date,
                Some("date-time") => PortType::Datetime,
                _ => PortType::String,
            },
            "integer" => PortType::Integer,
            "number" => PortType::Number,
            "boolean" => PortType::Boolean,
            "array" => match object.get("items") {
                Some(items @ Value::Object(_)) => PortType::List(Box::new(self.lower(
                    items,
                    &format!("{path}/items"),
                    depth.saturating_add(1),
                )?)),
                _ => return refuse(path, Reason::UntypedItems),
            },
            "object" => self.record(object, path, depth)?,
            other => return refuse(path, Reason::UnknownType(other.to_owned())),
        })
    }

    fn record(
        &mut self,
        object: &Map<String, Value>,
        path: &str,
        depth: usize,
    ) -> Result<PortType, Unsupported> {
        let additional = object.get("additionalProperties");
        let properties = object.get("properties").and_then(Value::as_object);
        let Some(properties) = properties.filter(|p| !p.is_empty()) else {
            // No named fields: a typed map, or nothing a port can check.
            return match additional {
                Some(value @ Value::Object(_)) => Ok(PortType::Map(Box::new(self.lower(
                    value,
                    &format!("{path}/additionalProperties"),
                    depth.saturating_add(1),
                )?))),
                _ => refuse(path, Reason::Untyped),
            };
        };
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = BTreeMap::new();
        for (name, schema) in properties {
            let port = self.lower(
                schema,
                &format!("{path}/properties/{}", escape(name)),
                depth.saturating_add(1),
            )?;
            fields.insert(
                name.clone(),
                Field {
                    port,
                    required: required.contains(&name.as_str()),
                },
            );
        }
        Ok(PortType::Record {
            fields,
            open: !matches!(additional, Some(Value::Bool(false))),
        })
    }
}

impl PortType {
    /// The port type as JSON: `{"kind": …}` with `values`, `item`, `value`,
    /// `inner`, or `fields` (`[{name, type, required}]`, sorted) and `open`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match self {
            Self::String => json!({"kind": "string"}),
            Self::Enum(values) => json!({"kind": "enum", "values": values}),
            Self::Integer => json!({"kind": "integer"}),
            Self::Number => json!({"kind": "number"}),
            Self::Boolean => json!({"kind": "boolean"}),
            Self::Date => json!({"kind": "date"}),
            Self::Datetime => json!({"kind": "datetime"}),
            Self::List(item) => json!({"kind": "list", "item": item.to_json()}),
            Self::Map(value) => json!({"kind": "map", "value": value.to_json()}),
            Self::Nullable(inner) => json!({"kind": "nullable", "inner": inner.to_json()}),
            Self::Record { fields, open } => json!({
                "kind": "record",
                "open": open,
                "fields": fields.iter().map(|(name, field)| json!({
                    "name": name, "type": field.port.to_json(), "required": field.required,
                })).collect::<Vec<_>>(),
            }),
        }
    }

    /// A short human name, for refusals.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::String => "string".to_owned(),
            Self::Enum(values) => format!("one of {}", values.join(" | ")),
            Self::Integer => "integer".to_owned(),
            Self::Number => "number".to_owned(),
            Self::Boolean => "boolean".to_owned(),
            Self::Date => "date".to_owned(),
            Self::Datetime => "datetime".to_owned(),
            Self::List(item) => format!("list<{}>", item.name()),
            Self::Map(value) => format!("map<{}>", value.name()),
            Self::Nullable(inner) => format!("{}?", inner.name()),
            Self::Record { .. } => "record".to_owned(),
        }
    }
}

/// Why an edge from one port to another is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRefusal {
    /// Dotted field path inside the target port (`""` for the port itself).
    pub path: String,
    /// `edge.type_mismatch`, `edge.missing_field` or `edge.nullable`.
    pub code: &'static str,
    /// The target's type at `path`.
    pub expected: String,
    /// The source's type at `path`, or `absent`.
    pub found: String,
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}.{name}")
    }
}

/// Whether a value of `source` may flow into `target`; `None` when it may.
///
/// Integers widen to numbers, enums to strings or to a superset enum, dates
/// and datetimes to strings. Lists and maps are covariant. A record source
/// must have every required target field, required; optional target fields
/// it has must fit. A nullable source needs a nullable target.
#[must_use]
pub fn edge_refusal(source: &PortType, target: &PortType) -> Option<EdgeRefusal> {
    refusal_at(source, target, "")
}

fn refusal_at(source: &PortType, target: &PortType, path: &str) -> Option<EdgeRefusal> {
    let mismatch = |code| {
        Some(EdgeRefusal {
            path: path.to_owned(),
            code,
            expected: target.name(),
            found: source.name(),
        })
    };
    match (source, target) {
        (PortType::Nullable(s), PortType::Nullable(t)) => refusal_at(s, t, path),
        (PortType::Nullable(_), _) => mismatch("edge.nullable"),
        (s, PortType::Nullable(t)) => refusal_at(s, t, path),
        (PortType::List(s), PortType::List(t)) | (PortType::Map(s), PortType::Map(t)) => {
            refusal_at(s, t, path)
        }
        (PortType::Enum(s), PortType::Enum(t)) => {
            if s.iter().all(|v| t.contains(v)) {
                None
            } else {
                mismatch("edge.type_mismatch")
            }
        }
        (PortType::Enum(_) | PortType::Date | PortType::Datetime, PortType::String)
        | (PortType::Integer, PortType::Number) => None,
        (PortType::Record { fields: s, .. }, PortType::Record { fields: t, .. }) => {
            t.iter().find_map(|(name, want)| {
                let at = join(path, name);
                match s.get(name) {
                    Some(have) if want.required && !have.required => Some(EdgeRefusal {
                        path: at,
                        code: "edge.missing_field",
                        expected: want.port.name(),
                        found: format!("optional {}", have.port.name()),
                    }),
                    Some(have) => refusal_at(&have.port, &want.port, &at),
                    None if want.required => Some(EdgeRefusal {
                        path: at,
                        code: "edge.missing_field",
                        expected: want.port.name(),
                        found: "absent".to_owned(),
                    }),
                    None => None,
                }
            })
        }
        (s, t) if s == t => None,
        _ => mismatch("edge.type_mismatch"),
    }
}

#[cfg(test)]
#[path = "tool_ports_tests.rs"]
mod tests;
