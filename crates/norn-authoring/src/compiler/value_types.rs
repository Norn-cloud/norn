//! Shared `EntityIR` v2 property types and pinned registry value types.
//!
//! Ports `tools/norn-ts/src/compiler/value-types.ts` and the `PropertyType`
//! representation from `tools/norn-ts/src/builder/entity.ts`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Wire spellings of the shared `norn_ir::Rounding` contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    /// Refuse inexact rounding.
    Refuse,
    /// Round toward zero.
    TowardZero,
    /// Round half to even.
    HalfEven,
    /// Round half away from zero.
    HalfAwayFromZero,
}

impl Rounding {
    /// Parses a wire rounding mode string.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "refuse" => Some(Self::Refuse),
            "toward_zero" => Some(Self::TowardZero),
            "half_even" => Some(Self::HalfEven),
            "half_away_from_zero" => Some(Self::HalfAwayFromZero),
            _ => None,
        }
    }

    /// Returns the canonical wire spelling.
    #[must_use]
    pub const fn wire_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::TowardZero => "toward_zero",
            Self::HalfEven => "half_even",
            Self::HalfAwayFromZero => "half_away_from_zero",
        }
    }
}

/// Optional semantic text format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextFormat {
    /// Email address.
    Email,
    /// URI.
    Uri,
    /// UUID.
    Uuid,
}

/// Timestamp timezone policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimezonePolicy {
    /// UTC instant.
    Utc,
    /// Explicit offset instant.
    Offset,
}

impl TimezonePolicy {
    /// Parses `"utc"` or `"offset"`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "utc" => Some(Self::Utc),
            "offset" => Some(Self::Offset),
            _ => None,
        }
    }

    /// Returns the canonical wire spelling.
    #[must_use]
    pub const fn wire_str(self) -> &'static str {
        match self {
            Self::Utc => "utc",
            Self::Offset => "offset",
        }
    }
}

/// Quantity unit reference (`ucum` or `qudt`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRef {
    /// Unit system (`"ucum"` or `"qudt"`).
    pub system: String,
    /// Unit code in `system`.
    pub code: String,
}

/// Authoritative currency source for a monetary decimal property.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencySource {
    /// Link hops to the currency holder (empty for a sibling property).
    pub links: Vec<String>,
    /// Property name holding the ISO-4217 currency code.
    pub property: String,
}

/// Single enum member declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumValue {
    /// Wire string value.
    pub value: String,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Numeric aggregation semantics (`METRIC-ADDITIVITY-1`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PropertyAdditivity {
    /// Undeclared additivity (omitted on the wire).
    Undeclared,
    /// Fully additive metric.
    Additive,
    /// Semi-additive snapshot metric.
    SemiAdditive,
    /// Non-additive metric.
    NonAdditive,
    /// Ratio of two sibling properties.
    Ratio {
        /// Numerator sibling property id.
        numerator: String,
        /// Denominator sibling property id.
        denominator: String,
    },
}

impl PropertyAdditivity {
    /// Converts to canonical JSON representation.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        match self {
            Self::Undeclared => {
                map.insert("kind".to_owned(), Value::String("undeclared".to_owned()));
            }
            Self::Additive => {
                map.insert("kind".to_owned(), Value::String("additive".to_owned()));
            }
            Self::SemiAdditive => {
                map.insert("kind".to_owned(), Value::String("semi_additive".to_owned()));
            }
            Self::NonAdditive => {
                map.insert("kind".to_owned(), Value::String("non_additive".to_owned()));
            }
            Self::Ratio {
                numerator,
                denominator,
            } => {
                map.insert("kind".to_owned(), Value::String("ratio".to_owned()));
                map.insert("numerator".to_owned(), Value::String(numerator.clone()));
                map.insert("denominator".to_owned(), Value::String(denominator.clone()));
            }
        }
        Value::Object(map)
    }
}

/// Exact decimal property descriptor (`kind: "decimal"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecimalType {
    /// Decimal fractional digit scale (`0..=28`).
    pub scale: u32,
    /// Optional physical unit reference.
    pub unit: Option<UnitRef>,
    /// Optional monetary currency source.
    pub currency_source: Option<CurrencySource>,
    /// Optional per-unit denominator for rate decimals (`VAL-RATE`).
    pub per_unit: Option<UnitRef>,
    /// Optional rounding contract.
    pub rounding: Option<Rounding>,
    /// Optional metric additivity declaration.
    pub additivity: Option<PropertyAdditivity>,
    /// Optional sibling unit-partitioning properties (`METRIC-UNIT-PARTITION-1`).
    pub unit_source: Vec<String>,
}

impl DecimalType {
    /// Serializes this decimal type to canonical `EntityIR` JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("kind".to_owned(), Value::String("decimal".to_owned()));
        map.insert("scale".to_owned(), Value::Number(self.scale.into()));
        if let Some(additivity) = &self.additivity {
            map.insert("additivity".to_owned(), additivity.to_json());
        }
        if let Some(unit) = &self.unit {
            let mut u = Map::new();
            u.insert("system".to_owned(), Value::String(unit.system.clone()));
            u.insert("code".to_owned(), Value::String(unit.code.clone()));
            map.insert("unit".to_owned(), Value::Object(u));
        }
        if let Some(cs) = &self.currency_source {
            let mut c = Map::new();
            c.insert(
                "links".to_owned(),
                Value::Array(cs.links.iter().cloned().map(Value::String).collect()),
            );
            c.insert("property".to_owned(), Value::String(cs.property.clone()));
            map.insert("currencySource".to_owned(), Value::Object(c));
        }
        if let Some(pu) = &self.per_unit {
            let mut p = Map::new();
            p.insert("system".to_owned(), Value::String(pu.system.clone()));
            p.insert("code".to_owned(), Value::String(pu.code.clone()));
            map.insert("perUnit".to_owned(), Value::Object(p));
        }
        if let Some(rounding) = self.rounding {
            map.insert(
                "rounding".to_owned(),
                Value::String(rounding.wire_str().to_owned()),
            );
        }
        if !self.unit_source.is_empty() {
            map.insert(
                "unitSource".to_owned(),
                Value::Array(
                    self.unit_source
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        Value::Object(map)
    }
}

/// `EntityIR` v2 property value kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropertyType {
    /// UTF-8 text, optionally formatted.
    Text {
        /// Optional semantic text format.
        format: Option<TextFormat>,
    },
    /// Exact integer.
    Integer {
        /// Optional metric additivity declaration.
        additivity: Option<PropertyAdditivity>,
    },
    /// Exact fixed-scale decimal (boxed to keep enum size compact).
    Decimal(Box<DecimalType>),
    /// Bound decimal group (`Norn.boundDecimal`).
    DecimalBinding {
        /// Decimal binding group name.
        group: String,
    },
    /// Boolean value.
    Boolean,
    /// RFC 3339 timestamp with timezone policy.
    Timestamp {
        /// Timezone policy (`utc` or `offset`).
        timezone: TimezonePolicy,
    },
    /// Calendar date (`YYYY-MM-DD`).
    Date,
    /// Duration value.
    Duration,
    /// Closed string enumeration.
    Enum {
        /// Allowed enumeration values in declaration order.
        values: Vec<EnumValue>,
    },
    /// Reference to a local or registry value type (`Norn.ref`).
    Ref {
        /// Value type name (for example `core.quantity`).
        name: String,
        /// Optional use-site additivity.
        additivity: Option<PropertyAdditivity>,
        /// Optional unit-partitioning sibling properties.
        unit_source: Vec<String>,
    },
}

impl PropertyType {
    /// Constructs a plain text property type.
    #[must_use]
    pub const fn text() -> Self {
        Self::Text { format: None }
    }

    /// Constructs a plain integer property type.
    #[must_use]
    pub const fn integer() -> Self {
        Self::Integer { additivity: None }
    }

    /// Constructs a plain fixed-scale decimal property type.
    #[must_use]
    pub fn decimal(scale: u32, unit: Option<UnitRef>) -> Self {
        Self::Decimal(Box::new(DecimalType {
            scale,
            unit,
            currency_source: None,
            per_unit: None,
            rounding: None,
            additivity: None,
            unit_source: Vec::new(),
        }))
    }

    /// Constructs a monetary decimal property type.
    #[must_use]
    pub fn money(scale: u32, currency_property: String) -> Self {
        Self::Decimal(Box::new(DecimalType {
            scale,
            unit: None,
            currency_source: Some(CurrencySource {
                links: Vec::new(),
                property: currency_property,
            }),
            per_unit: None,
            rounding: None,
            additivity: None,
            unit_source: Vec::new(),
        }))
    }

    /// Constructs a currency-per-unit rate decimal property type.
    #[must_use]
    pub fn rate(
        scale: u32,
        currency_property: String,
        per_unit: UnitRef,
        rounding: Option<Rounding>,
    ) -> Self {
        Self::Decimal(Box::new(DecimalType {
            scale,
            unit: None,
            currency_source: Some(CurrencySource {
                links: Vec::new(),
                property: currency_property,
            }),
            per_unit: Some(per_unit),
            rounding,
            additivity: None,
            unit_source: Vec::new(),
        }))
    }

    /// Constructs a string enumeration property type.
    #[must_use]
    pub fn enumeration(values: Vec<String>) -> Self {
        Self::Enum {
            values: values
                .into_iter()
                .map(|value| EnumValue {
                    value,
                    description: None,
                })
                .collect(),
        }
    }

    /// Returns the wire `kind` discriminator name.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Integer { .. } => "integer",
            Self::Decimal(_) => "decimal",
            Self::DecimalBinding { .. } => "decimal-binding",
            Self::Boolean => "boolean",
            Self::Timestamp { .. } => "timestamp",
            Self::Date => "date",
            Self::Duration => "duration",
            Self::Enum { .. } => "enum",
            Self::Ref { .. } => "ref",
        }
    }

    /// Serializes this property type to canonical `EntityIR` JSON.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = Map::new();
        match self {
            Self::Text { format } => {
                map.insert("kind".to_owned(), Value::String("text".to_owned()));
                if let Some(fmt) = format {
                    let s = match fmt {
                        TextFormat::Email => "email",
                        TextFormat::Uri => "uri",
                        TextFormat::Uuid => "uuid",
                    };
                    map.insert("format".to_owned(), Value::String(s.to_owned()));
                }
            }
            Self::Integer { additivity } => {
                map.insert("kind".to_owned(), Value::String("integer".to_owned()));
                if let Some(add) = additivity {
                    map.insert("additivity".to_owned(), add.to_json());
                }
            }
            Self::Decimal(dec) => return dec.to_json(),
            Self::DecimalBinding { group } => {
                map.insert(
                    "kind".to_owned(),
                    Value::String("decimal-binding".to_owned()),
                );
                map.insert("group".to_owned(), Value::String(group.clone()));
            }
            Self::Boolean => {
                map.insert("kind".to_owned(), Value::String("boolean".to_owned()));
            }
            Self::Timestamp { timezone } => {
                map.insert("kind".to_owned(), Value::String("timestamp".to_owned()));
                map.insert(
                    "timezone".to_owned(),
                    Value::String(timezone.wire_str().to_owned()),
                );
            }
            Self::Date => {
                map.insert("kind".to_owned(), Value::String("date".to_owned()));
            }
            Self::Duration => {
                map.insert("kind".to_owned(), Value::String("duration".to_owned()));
            }
            Self::Enum { values } => {
                map.insert("kind".to_owned(), Value::String("enum".to_owned()));
                let arr = values
                    .iter()
                    .map(|item| {
                        let mut obj = Map::new();
                        obj.insert("value".to_owned(), Value::String(item.value.clone()));
                        if let Some(desc) = &item.description {
                            obj.insert("description".to_owned(), Value::String(desc.clone()));
                        }
                        Value::Object(obj)
                    })
                    .collect();
                map.insert("values".to_owned(), Value::Array(arr));
            }
            Self::Ref {
                name,
                additivity,
                unit_source,
            } => {
                map.insert("kind".to_owned(), Value::String("ref".to_owned()));
                map.insert("name".to_owned(), Value::String(name.clone()));
                if let Some(add) = additivity
                    && !matches!(add, PropertyAdditivity::Undeclared)
                {
                    map.insert("additivity".to_owned(), add.to_json());
                }
                if !unit_source.is_empty() {
                    map.insert(
                        "unitSource".to_owned(),
                        Value::Array(unit_source.iter().cloned().map(Value::String).collect()),
                    );
                }
            }
        }
        Value::Object(map)
    }
}

/// Resolves a pinned registry value type name (`core.*`, `iso8601.*`, etc.),
/// returning `None` for local or unknown names.
///
/// Mirrors `REGISTRY_VALUE_TYPES` in `tools/norn-ts/src/compiler/value-types.ts`.
#[must_use]
pub(crate) fn registry_value_type(name: &str) -> Option<PropertyType> {
    match name {
        "core.display_name"
        | "core.code"
        | "core.document_number"
        | "core.email_address"
        | "core.phone_number"
        | "core.national_id"
        | "core.bank_account_number"
        | "ucum.unit_code"
        | "qudt.unit_id"
        | "gs1.gtin"
        | "gs1.gln"
        | "lei.lei"
        | "unlocode.location_code"
        | "isco08.occupation_code"
        | "esco.occupation_uri" => Some(PropertyType::text()),
        "core.money_amount" | "core.quantity" => Some(PropertyType::decimal(6, None)),
        "iso8601.date" => Some(PropertyType::Date),
        "iso8601.timestamp" => Some(PropertyType::Timestamp {
            timezone: TimezonePolicy::Utc,
        }),
        _ => None,
    }
}
