//! Typed Setting root: the payload of a `setting` artifact (T1-IR-1e, #2820;
//! typed value T1-IR-2, #2430).
//!
//! A setting is one reviewed, effective-dated configuration value under a
//! stable key. The root mirrors today's validator exactly: it is closed at the
//! top, and its nested objects keep the members the validator does not check
//! in a flattened `additional` map (see [`crate::members`]).
//!
//! `value` is a [`SettingValue`], a union tagged by the wire `valueType`: a
//! `text` setting carries a string, `number` a finite number, `boolean` a
//! boolean, and `json` any JSON value including `null`. The wire keeps the
//! two members side by side (`valueType`, `value`), so bytes and hashes are
//! unchanged; a disagreement is a decode error here and the validator's
//! diagnostic in `norn-checker`. The `effectiveTo > effectiveFrom` ordering
//! stays a checker rule.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::JsonValue;
use crate::members::{IrVersion01, present_non_null};
use crate::rule::JsonObject;

/// Typed payload of a `setting` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SettingRootWire", into = "SettingRootWire")]
pub struct SettingRoot {
    pub setting_version: IrVersion01,
    pub setting_key: String,
    /// The value, tagged by its wire `valueType`.
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
    /// `valueType: "number"`: a finite binary64 number.
    Number(f64),
    /// `valueType: "boolean"`: a boolean.
    Boolean(bool),
    /// `valueType: "json"`: any JSON value, including `null`.
    Json(JsonValue),
}

impl SettingValue {
    /// Pair a wire `valueType` with its `value`, or `None` when the value's
    /// JSON type disagrees with `valueType` (or a number is not finite).
    #[must_use]
    pub fn from_wire(value_type: SettingValueType, value: JsonValue) -> Option<Self> {
        match (value_type, value) {
            (SettingValueType::Text, JsonValue::String(text)) => Some(Self::Text(text)),
            (SettingValueType::Number, JsonValue::Number(number)) if number.is_finite() => {
                Some(Self::Number(number))
            }
            (SettingValueType::Boolean, JsonValue::Bool(flag)) => Some(Self::Boolean(flag)),
            (SettingValueType::Json, value) => Some(Self::Json(value)),
            _ => None,
        }
    }

    /// The wire `valueType` tag of this value.
    #[must_use]
    pub const fn value_type(&self) -> SettingValueType {
        match self {
            Self::Text(_) => SettingValueType::Text,
            Self::Number(_) => SettingValueType::Number,
            Self::Boolean(_) => SettingValueType::Boolean,
            Self::Json(_) => SettingValueType::Json,
        }
    }

    /// The wire `value`.
    #[must_use]
    pub fn to_json_value(&self) -> JsonValue {
        match self {
            Self::Text(text) => JsonValue::String(text.clone()),
            Self::Number(number) => JsonValue::Number(*number),
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
    setting_version: IrVersion01,
    setting_key: String,
    value_type: SettingValueType,
    /// The value, whose JSON type agrees with `valueType`. A `json` setting
    /// may be `null`.
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
        let value = SettingValue::from_wire(value_type, wire.value).ok_or_else(|| {
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
            generator.subschema_for::<f64>(),
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
                branch(SettingValueType::Number, number),
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

/// The JSON type of a setting's `value`.
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
