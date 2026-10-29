//! Member types shared by several typed payload roots (T1-IR-1d, #2509).
//!
//! The typed roots mirror the shape today's `norn-checker` validators accept,
//! exactly. Two consequences recur across roots and live here:
//!
//! - **Omitted vs `null`.** A member the validator treats as "optional, but
//!   must be a string/object/boolean when present" rejects an explicit `null`;
//!   `present_non_null` decodes such a member (omitted → `None`, `null` →
//!   error). Members the validator does not check at all keep any JSON,
//!   including `null`, as `Option<JsonValue>` decoded through the same
//!   function (`null` → `Some(JsonValue::Null)`).
//! - **Open nested objects.** The validators check named members of nested
//!   objects but do not reject unknown ones, so every nested typed struct keeps
//!   its unchecked members verbatim in a flattened `additional` map. Closing
//!   them is a validator behaviour change and is tracked separately from the
//!   root members (T1-IR-2 typed every root member; see the crate docs).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::JsonValue;
use crate::rule::JsonObject;

/// Decode a member that may be omitted but must not be an explicit `null`.
///
/// Used with `#[serde(default, deserialize_with = "present_non_null")]`: an
/// omitted member takes the `None` default and a present one decodes as `T`,
/// so `null` is an error unless `T` itself accepts it (`JsonValue` does).
pub(crate) fn present_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// A non-negative integral JSON number.
///
/// IR JSON numbers are binary64 ([`JsonValue::Number`]), so a count written
/// as `3` and one decoded as `3.0` are the same value. This accepts any
/// integral, non-negative number up to 2^53 (the exact-integer range of
/// binary64), matching the validators' `fract() == 0.0` checks; each member's
/// lower bound stays a checker rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct WholeNumber(#[schemars(range(min = 0, max = 9_007_199_254_740_992_u64))] pub u64);

impl<'de> Deserialize<'de> for WholeNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        const MAX_EXACT: f64 = 9_007_199_254_740_992.0;
        // Through `JsonValue`, so the number rule is the IR's under both
        // `serde_json` number profiles (issue #2440).
        match JsonValue::deserialize(deserializer)? {
            JsonValue::Number(value)
                if value.is_finite()
                    && value.fract() == 0.0
                    && (0.0..=MAX_EXACT).contains(&value) =>
            {
                #[allow(
                    clippy::as_conversions,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "value is a non-negative integer no larger than 2^53"
                )]
                Ok(Self(value as u64))
            }
            JsonValue::Null
            | JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::String(_)
            | JsonValue::Array(_)
            | JsonValue::Object(_) => Err(serde::de::Error::custom(
                "expected a non-negative integer no larger than 2^53",
            )),
        }
    }
}

/// Decode an exact IR whole number in the unsigned 32-bit range.
pub(crate) fn deserialize_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u32::try_from(WholeNumber::deserialize(deserializer)?.0).map_err(serde::de::Error::custom)
}

/// Publish an untagged enum of provably disjoint forms as `oneOf` rather than
/// schemars' default `anyOf`, so a payload matches exactly one form.
///
/// Used as `#[schemars(transform = one_of_forms)]` on roots whose forms are
/// disjoint by construction (each form is closed and differs in a required
/// literal or member).
pub(crate) fn one_of_forms(schema: &mut schemars::Schema) {
    if let Some(forms) = schema.remove("anyOf") {
        schema.insert("oneOf".to_owned(), forms);
    }
}

/// The JSON literal `false`: a capability flag that must stay off.
///
/// Deterministic `RuleIR` may carry `sideEffects: false`; any other value is
/// rejected (the checker reports it as a disallowed side effect).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct JsonFalse;

impl Serialize for JsonFalse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for JsonFalse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? {
            Err(serde::de::Error::custom("expected the literal false"))
        } else {
            Ok(Self)
        }
    }
}

impl JsonSchema for JsonFalse {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonFalse".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "const": false })
    }
}

/// An always-empty JSON array (`[]`): a list that must stay empty, such as
/// the services a deterministic `RuleIR` rule may require.
pub type EmptyList = [String; 0];

/// Preview-proposal metadata carried by agent- or user-proposed connector and
/// widget definitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProposalMetadata {
    pub mode: ProposalMode,
    pub proposed_by: ProposalActor,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub rationale: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub source_artifact_id: Option<String>,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// How a proposal may be applied. Only previews exist today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalMode {
    Preview,
}

/// Who proposed a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalActor {
    Agent,
    User,
}

/// The only IR `0.1` version literal of the connector, widget, customization,
/// policy, derived, and setting roots and of ontology business packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum IrVersion01 {
    #[default]
    #[serde(rename = "0.1")]
    V0_1,
}
