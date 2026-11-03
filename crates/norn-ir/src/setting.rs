//! Typed Setting root: the payload of a `setting` artifact (T1-IR-1e, #2820;
//! typed value T1-IR-2, #2430).
//!
//! A setting is one reviewed, effective-dated configuration value under a
//! stable key. The root mirrors today's validator exactly: it is closed at the
//! top, and its nested objects keep the members the validator does not check
//! in a flattened `additional` map (see [`crate::members`]).
//!
//! `value` is a [`SettingValue`], a union tagged by the wire `valueType`:
//! `number` settings use the shared exact decimal-string literal on the wire.
//! This keeps economic values out of definition JSON's binary64 representation;
//! execution decodes them to exact runtime numbers. New 0.2 numeric JSON tokens refuse.
//! PARAM-EXACT-1: numeric settings retain their literal through admission and
//! decode to the common exact runtime profile without rounding. Other value
//! kinds retain their existing value representation. PARAM-WIRE-2 keeps retained
//! 0.1 values tagged as binary64; they never enter exact-decimal execution.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion02, present_non_null};
use crate::rule::JsonObject;
use crate::{Decimal, DecimalLiteral, JsonValue, RuntimeValue, RuntimeValueError};

/// Typed payload of a `setting` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SettingRootWire", into = "SettingRootWire")]
pub struct SettingRoot {
    pub setting_version: IrVersion02,
    pub setting_key: String,
    /// The wire value paired with `valueType`: version 0.2 numbers require
    /// exact decimal-string literals; retained 0.1 keeps its binary64 profile.
    /// A `json` setting may be `null`.
    pub value: SettingValue,
    pub effective_from: String,
    /// Must sort after `effectiveFrom` (checker rule).
    pub effective_to: Option<String>,
    /// The setting version this one replaces.
    pub supersedes: Option<SettingSupersedes>,
    pub review: SettingReview,
    /// Scope dimensions the caller must supply (fail closed).
    pub scope_required: Vec<String>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    pub extensions: Option<JsonObject>,
}

/// A setting value, tagged by its `valueType`.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// `valueType: "text"`: a string.
    Text(String),
    /// `valueType: "number"`: an exact decimal-string literal.
    Number(DecimalLiteral),
    /// Historical `settingVersion: "0.1"` binary64 value; never an exact literal.
    LegacyNumber(f64),
    /// `valueType: "boolean"`: a boolean.
    Boolean(bool),
    /// `valueType: "json"`: any JSON value, including `null`.
    Json(JsonValue),
}

impl SettingValue {
    /// Pair a wire `valueType` with its `value`, or `None` when the value's
    /// wire encoding disagrees with `valueType` or a decimal literal is inexact.
    #[must_use]
    pub fn from_wire(value_type: SettingValueType, value: JsonValue) -> Option<Self> {
        match (value_type, value) {
            (SettingValueType::Text, JsonValue::String(text)) => Some(Self::Text(text)),
            (SettingValueType::Number, JsonValue::String(text)) => {
                Self::decimal(&text).ok()?;
                Some(Self::Number(DecimalLiteral(text)))
            }
            (SettingValueType::Boolean, JsonValue::Bool(flag)) => Some(Self::Boolean(flag)),
            (SettingValueType::Json, value) => Some(Self::Json(value)),
            _ => None,
        }
    }

    /// Decode the common decimal literal without a binary floating-point step.
    ///
    /// # Errors
    /// Refuses non-literal syntax and the shared exact decimal range/scale overflow.
    pub fn decimal(text: &str) -> Result<Decimal, RuntimeValueError> {
        if text
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && byte != b'-' && byte != b'.')
        {
            return Err(RuntimeValueError::Inexact {
                number: text.into(),
            });
        }
        Decimal::parse(text)
    }

    /// The value supplied to the evaluator.
    ///
    /// # Errors
    /// Refuses invalid constructed decimal literals or unrepresentable JSON numbers.
    pub fn runtime_value(&self) -> Result<RuntimeValue, RuntimeValueError> {
        match self {
            Self::Number(literal) => Self::decimal(literal.as_str()).map(RuntimeValue::Number),
            Self::LegacyNumber(value) => Err(RuntimeValueError::Inexact {
                number: value.to_string(),
            }),
            value @ (Self::Text(_) | Self::Boolean(_) | Self::Json(_)) => {
                RuntimeValue::from_ir(&value.to_json_value())
            }
        }
    }

    /// The wire `valueType` tag of this value.
    #[must_use]
    pub const fn value_type(&self) -> SettingValueType {
        match self {
            Self::Text(_) => SettingValueType::Text,
            Self::Number(_) | Self::LegacyNumber(_) => SettingValueType::Number,
            Self::Boolean(_) => SettingValueType::Boolean,
            Self::Json(_) => SettingValueType::Json,
        }
    }

    /// The wire `value`.
    #[must_use]
    pub fn to_json_value(&self) -> JsonValue {
        match self {
            Self::Text(text) => JsonValue::String(text.clone()),
            Self::Number(number) => JsonValue::String(number.0.clone()),
            Self::LegacyNumber(number) => JsonValue::Number(*number),
            Self::Boolean(flag) => JsonValue::Bool(*flag),
            Self::Json(value) => value.clone(),
        }
    }
}

/// The wire form of [`SettingRoot`]: `valueType` and `value` side by side.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename = "SettingRoot")]
struct SettingRootWire {
    setting_version: IrVersion02,
    setting_key: String,
    value_type: SettingValueType,
    /// Version 0.2 numbers require exact decimal-string literals; retained 0.1 numbers
    /// keep binary64 JSON numbers. A `json` setting may be `null`.
    value: JsonValue,
    effective_from: String,
    /// Must sort after `effectiveFrom` (checker rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    effective_to: Option<String>,
    /// The setting version this one replaces.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "SettingSupersedes")]
    supersedes: Option<SettingSupersedes>,
    review: SettingReview,
    /// Scope dimensions the caller must supply (fail closed).
    scope_required: Vec<String>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    extensions: Option<JsonObject>,
}

impl TryFrom<SettingRootWire> for SettingRoot {
    type Error = String;

    fn try_from(wire: SettingRootWire) -> Result<Self, Self::Error> {
        let value_type = wire.value_type;
        let value = match (wire.setting_version, value_type, wire.value) {
            (IrVersion02::V0_1, SettingValueType::Number, JsonValue::Number(n))
                if n.is_finite() =>
            {
                Some(SettingValue::LegacyNumber(n))
            }
            (IrVersion02::V0_1, SettingValueType::Number, _) => None,
            (_, kind, value) => SettingValue::from_wire(kind, value),
        }
        .ok_or_else(|| {
            format!(
                "value does not match valueType '{}'",
                value_type.as_wire_str()
            )
        })?;
        Ok(Self {
            setting_version: wire.setting_version,
            setting_key: wire.setting_key,
            value,
            effective_from: wire.effective_from,
            effective_to: wire.effective_to,
            supersedes: wire.supersedes,
            review: wire.review,
            scope_required: wire.scope_required,
            extensions: wire.extensions,
        })
    }
}

impl From<SettingRoot> for SettingRootWire {
    fn from(root: SettingRoot) -> Self {
        Self {
            setting_version: root.setting_version,
            setting_key: root.setting_key,
            value_type: root.value.value_type(),
            value: root.value.to_json_value(),
            effective_from: root.effective_from,
            effective_to: root.effective_to,
            supersedes: root.supersedes,
            review: root.review,
            scope_required: root.scope_required,
            extensions: root.extensions,
        }
    }
}

impl JsonSchema for SettingRoot {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SettingRoot".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::SettingRoot").into()
    }

    /// The wire object, plus one `oneOf` branch per `valueType` that fixes the
    /// JSON type of `value`.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = SettingRootWire::json_schema(generator);
        let [text, number, boolean, json] = [
            generator.subschema_for::<String>(),
            generator.subschema_for::<DecimalLiteral>(),
            generator.subschema_for::<bool>(),
            generator.subschema_for::<JsonValue>(),
        ];
        let branch = |value_type: SettingValueType, value: schemars::Schema| {
            schemars::json_schema!({
                "properties": {
                    "valueType": { "const": value_type.as_wire_str() },
                    "value": value,
                },
            })
        };
        let mut one_of = schemars::json_schema!({
            "oneOf": [
                branch(SettingValueType::Text, text),
                {
                    "properties": {
                        "settingVersion": { "const": "0.2" },
                        "valueType": { "const": "number" }, "value": number,
                    },
                },
                {
                    "properties": {
                        "settingVersion": { "const": "0.1" },
                        "valueType": { "const": "number" }, "value": { "type": "number" },
                    },
                },
                branch(SettingValueType::Boolean, boolean),
                branch(SettingValueType::Json, json),
            ],
        });
        if let Some(branches) = one_of.remove("oneOf") {
            schema.insert("oneOf".to_owned(), branches);
        }
        schema
    }
}

/// The setting value kind; numbers use exact decimal-string literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingValueType {
    Text,
    Number,
    Boolean,
    Json,
}

impl SettingValueType {
    /// The wire spelling (`text`, `number`, `boolean`, `json`).
    #[must_use]
    pub const fn as_wire_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Json => "json",
        }
    }
}

/// The exact setting artifact version a setting supersedes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingSupersedes {
    pub artifact_id: String,
    pub version: String,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Review state of a setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingReview {
    pub state: SettingReviewState,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub reviewed_by: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub reviewed_at: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub reason: Option<String>,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Review states of a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingReviewState {
    Draft,
    Proposed,
    Approved,
    Active,
    Rejected,
    Superseded,
}
