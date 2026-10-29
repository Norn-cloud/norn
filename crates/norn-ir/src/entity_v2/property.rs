//! Shared property types (T1-ONT-1a, #2511): the value vocabulary of EntityIR
//! v2 (ruling 2026-09-09 §5: "value types with units, formats, enums, and
//! constraints").
//!
//! A [`PropertyType`] is one of eleven kinds. A [`PropertyTypeDef`] gives a type
//! and its [`PropertyConstraints`] a [`PropertyTypeName`], so object types
//! (T1-ONT-1b..d) reuse it by name through the `ref` kind instead of
//! restating it.
//!
//! Rules that shape the wire:
//!
//! - **Exact numbers only.** There is no binary floating-point kind. A
//!   `decimal` declares its `scale`, and every numeric bound is an exact
//!   [`DecimalLiteral`] string, never a JSON number, so bounds never pass
//!   through `f64` or `serde_json`'s number profiles (OPEN-DECISIONS D37).
//!   Instance-value wire encoding belongs to T3-NUM-1 (#2770).
//! - **No ambient timezone.** A `timestamp` names its [`TimezonePolicy`]; a
//!   local, zone-less date-time is not a value kind. A `date` is a calendar
//!   date with no zone.
//! - **Standards are referenced, not re-implemented.** Units are UCUM or QUDT
//!   codes ([`UnitRef`]); ISO 4217 currencies, ISO 3166 countries, GS1
//!   identifiers, and the other Norn Core value types (ruling 2026-09-25 §27)
//!   are registry property types a `ref` names.
//! - **Object references are links, not properties.** `ref` names a property
//!   type; a reference to another object is a link (T1-ONT-1d), as in Gel and
//!   Foundry.
//! - **Omitted vs `null`** follows #2819: every optional member may be
//!   omitted, and an explicit `null` is rejected. Unknown members are
//!   rejected.
//!
//! Every consistency rule (name grammar, unique names, resolvable and acyclic
//! refs, enum uniqueness, bound grammar and order, constraint applicability,
//! pattern compilation) is a `norn-checker` diagnostic, not a decode error.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MarkingId;
use crate::members::{WholeNumber, present_non_null};
use crate::rule::JsonObject;

/// Grammar of a [`PropertyTypeName`] (published as the schema `pattern`;
/// `norn-checker` enforces it).
pub const PROPERTY_TYPE_NAME_PATTERN: &str = r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$";

/// Grammar of a [`DecimalLiteral`] (published as the schema `pattern`;
/// `norn-checker` enforces it).
pub const DECIMAL_LITERAL_PATTERN: &str = r"^-?(0|[1-9][0-9]*)(\.[0-9]+)?$";

/// Dotted, lower-case name of a shared property type, for example
/// `commerce.sku_code` or the registry value type `iso4217.currency_code`.
///
/// Grammar (checked by `norn-checker`): one or more `.`-separated segments,
/// each `[a-z][a-z0-9_]*`.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct PropertyTypeName(#[schemars(regex(pattern = PROPERTY_TYPE_NAME_PATTERN))] pub String);

impl PropertyTypeName {
    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An exact decimal number written as a JSON string: an optional `-`, an
/// integer part without leading zeros, and an optional fraction
/// (`"0"`, `"-12"`, `"19.990"`). No exponent, no `+`.
///
/// Trailing fraction zeros are kept: they carry the literal's scale.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct DecimalLiteral(#[schemars(regex(pattern = DECIMAL_LITERAL_PATTERN))] pub String);

impl DecimalLiteral {
    /// The literal as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A reusable, named property type: the unit of reuse across object types
/// (Foundry "shared property type", LinkML `type`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertyTypeDef {
    /// Unique name within the artifact set (and against registry names).
    pub name: PropertyTypeName,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The value kind.
    #[serde(rename = "type")]
    pub property_type: PropertyType,
    /// Constraints on values of this type. Omitted when empty.
    #[serde(default, skip_serializing_if = "PropertyConstraints::is_empty")]
    pub constraints: PropertyConstraints,
    /// Classification markings applied to every property using this type.
    /// Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
}

/// The value kind of a property, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PropertyType {
    /// A Unicode string, optionally in a well-known [`TextFormat`].
    Text {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "TextFormat")]
        format: Option<TextFormat>,
    },
    /// A signed 64-bit integer.
    Integer {},
    /// An exact decimal with a fixed number of fraction digits, optionally a
    /// quantity in a declared unit.
    Decimal {
        /// Fraction digits every value is held at (`2` for cents). At most 28,
        /// the exact range of ZEN Expression's decimal (checked by
        /// `norn-checker`).
        scale: WholeNumber,
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "UnitRef")]
        unit: Option<UnitRef>,
        /// Authoritative currency path relative to the object holding this amount.
        /// Monetary decimals declare this instead of a quantity unit. Metrics
        /// must verify this source; descriptions and names carry no semantics.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "crate::metric::DimensionPath")]
        currency_source: Option<crate::metric::DimensionPath>,
    },
    /// `true` or `false`.
    Boolean {},
    /// An instant in time with an explicit timezone policy.
    Timestamp {
        /// How the offset is held. Required: there is no ambient timezone.
        timezone: TimezonePolicy,
    },
    /// An ISO 8601 calendar date (`YYYY-MM-DD`) with no time and no zone.
    Date {},
    /// An exact elapsed duration (fixed-length units only; calendar periods
    /// such as months are not durations).
    Duration {},
    /// One of a closed set of string values.
    Enum {
        /// Declared values, in presentation order; unique and non-empty.
        values: Vec<EnumValue>,
    },
    /// A closed structured value. Every member is present; nullability of
    /// the containing property applies to the whole record.
    Record { members: Vec<RecordMember> },
    /// Opaque JSON (an extension payload), optionally described by a JSON
    /// Schema document the checker does not interpret.
    JsonExt {
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "JsonObject")]
        schema: Option<JsonObject>,
    },
    /// The shared property type (local or registry) with this name.
    Ref {
        /// The referenced [`PropertyTypeDef`] name.
        name: PropertyTypeName,
    },
}

impl PropertyType {
    /// The wire `kind` tag.
    #[must_use]
    pub const fn kind(&self) -> PropertyKind {
        match self {
            Self::Text { .. } => PropertyKind::Text,
            Self::Integer {} => PropertyKind::Integer,
            Self::Decimal { .. } => PropertyKind::Decimal,
            Self::Boolean {} => PropertyKind::Boolean,
            Self::Timestamp { .. } => PropertyKind::Timestamp,
            Self::Date {} => PropertyKind::Date,
            Self::Duration {} => PropertyKind::Duration,
            Self::Enum { .. } => PropertyKind::Enum,
            Self::Record { .. } => PropertyKind::Record,
            Self::JsonExt { .. } => PropertyKind::JsonExt,
            Self::Ref { .. } => PropertyKind::Ref,
        }
    }
}

/// The `kind` tag of a [`PropertyType`], without its members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PropertyKind {
    Text,
    Integer,
    Decimal,
    Boolean,
    Timestamp,
    Date,
    Duration,
    Enum,
    JsonExt,
    Record,
    Ref,
}

impl PropertyKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 11] = [
        Self::Text,
        Self::Integer,
        Self::Decimal,
        Self::Boolean,
        Self::Timestamp,
        Self::Date,
        Self::Duration,
        Self::Enum,
        Self::JsonExt,
        Self::Record,
        Self::Ref,
    ];

    /// The wire `kind` tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Boolean => "boolean",
            Self::Timestamp => "timestamp",
            Self::Date => "date",
            Self::Duration => "duration",
            Self::Enum => "enum",
            Self::JsonExt => "json-ext",
            Self::Record => "record",
            Self::Ref => "ref",
        }
    }
}

/// One required member of a closed record. A nullable member must still be
/// present, with an explicit null when it has no value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordMember {
    /// Unique member name, using the property id grammar.
    pub id: super::object::PropertyId,
    /// Inline member kind or a reference to a shared property type.
    #[serde(rename = "type")]
    pub property_type: PropertyType,
    /// Constraints on this required member, including its nullability.
    #[serde(default, skip_serializing_if = "PropertyConstraints::is_empty")]
    pub constraints: PropertyConstraints,
}

/// Well-known string formats of a `text` property.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TextFormat {
    /// An RFC 5322 address (`addr-spec`).
    Email,
    /// An RFC 3986 URI.
    Uri,
    /// An RFC 9562 UUID in its hyphenated text form.
    Uuid,
}

/// A timestamp's declared source-offset policy. Both policies require an
/// RFC 3339 date-time with an explicit offset; neither admits a local
/// date-time. Runtime storage canonicalizes both to UTC (#3112); raw source
/// observations retain the original offset as source evidence.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TimezonePolicy {
    /// An instant normalized to UTC (`Z`); the source offset is not kept.
    Utc,
    /// Source data supplies an offset. The runtime stores the instant in UTC;
    /// the original source representation belongs to raw observations.
    Offset,
}

/// A reference to a unit of measure in a standard unit vocabulary (ruling
/// 2026-09-25 §27). The code is not interpreted here; resolving it against the
/// vocabulary is registry work (T8-STD-1).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnitRef {
    pub system: UnitSystem,
    /// The unit's code in `system` (UCUM `kg`, `m/s`; QUDT `unit:KiloGM`).
    pub code: String,
}

/// A standard unit vocabulary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum UnitSystem {
    /// The Unified Code for Units of Measure (case-sensitive codes).
    Ucum,
    /// A QUDT unit (`unit:` local name).
    Qudt,
}

/// One declared value of an `enum` property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnumValue {
    /// The stored value; stable once published.
    pub value: String,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
}

/// Constraints on the values of a property type.
///
/// Applicability (checked by `norn-checker`): `min`/`max` apply to `integer`
/// and `decimal`; `minLength`/`maxLength`/`pattern` apply to `text`;
/// `nullable` applies to every kind. Through a `ref` they apply to the
/// referenced type's kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertyConstraints {
    /// Whether a value may be absent (`null`). Defaults to `false`; the
    /// canonical form omits `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub nullable: bool,
    /// Inclusive lower bound.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "DecimalLiteral")]
    pub min: Option<DecimalLiteral>,
    /// Inclusive upper bound.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "DecimalLiteral")]
    pub max: Option<DecimalLiteral>,
    /// Minimum length in Unicode scalar values.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WholeNumber")]
    pub min_length: Option<WholeNumber>,
    /// Maximum length in Unicode scalar values.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WholeNumber")]
    pub max_length: Option<WholeNumber>,
    /// A regular expression the value must contain a match for (JSON Schema
    /// `pattern` semantics: unanchored; write `^…$` for a full match). The
    /// dialect is `regex-lite`, the ZEN Expression pure profile's engine.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub pattern: Option<String>,
}

impl PropertyConstraints {
    /// Whether no constraint is declared (the canonical form omits it).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.nullable
            && self.min.is_none()
            && self.max.is_none()
            && self.min_length.is_none()
            && self.max_length.is_none()
            && self.pattern.is_none()
    }
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde `skip_serializing_if` passes a reference"
)]
pub(crate) const fn is_false(value: &bool) -> bool {
    !*value
}
