//! Closed `EntityIR` v2 property specifications, field defaults/metadata, links,
//! identity keys, masking specs, markings catalogs, and function handles.

use std::collections::BTreeMap;

use crate::canonical::{EnvelopeHeader, EnvelopeOptions, JsonValue, build_envelope};
use crate::decimal::format_exact_decimal;
use crate::error::{CODE_SCHEMA, PyAuthoringError, SourceSpan};
use crate::schema::types::{
    MarkingSeverity, Multiplicity, Rounding, TextFormat, TimezonePolicy, UnitRef,
};

/// Closed `EntityIR` v2 property type specification (never constructed from raw JSON dicts).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropertyTypeSpec {
    /// UTF-8 text property, optionally format-qualified.
    Text {
        /// Optional text format qualifier.
        format: Option<TextFormat>,
    },
    /// Exact arbitrary-precision integer property.
    Integer,
    /// Boolean property.
    Boolean,
    /// `ISO-8601` calendar date property.
    Date,
    /// `RFC-3339` timestamp property.
    Timestamp {
        /// Timezone policy (`"utc"` or `"offset"`).
        timezone: TimezonePolicy,
    },
    /// Exact duration property.
    Duration,
    /// Named value-type reference (`ref_type("core.display_name")`).
    Ref {
        /// Referenced value-type name.
        name: String,
    },
    /// Fixed-scale exact decimal, quantity, money, or rate property.
    Decimal {
        /// Fixed decimal scale (non-negative).
        scale: u64,
        /// Optional measurement unit (`Quantity`).
        unit: Option<UnitRef>,
        /// Optional currency source property name (`Money` or `Rate`).
        currency: Option<String>,
        /// Optional denominator unit (`Rate`).
        per_unit: Option<UnitRef>,
        /// Optional rounding mode.
        rounding: Option<Rounding>,
    },
    /// Closed string enumeration (`StrEnum`).
    Enum {
        /// Python `StrEnum` class name (for annotation consistency checks).
        class_name: String,
        /// Ordered enum member wire values.
        values: Vec<String>,
    },
}

impl PropertyTypeSpec {
    /// Constructs a text [`PropertyTypeSpec`].
    #[must_use]
    pub const fn text(format: Option<TextFormat>) -> Self {
        Self::Text { format }
    }

    /// Constructs a named reference [`PropertyTypeSpec`].
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] if `name` is empty.
    pub fn ref_type(
        name: impl Into<String>,
        span: Option<&SourceSpan>,
    ) -> Result<Self, PyAuthoringError> {
        let name = name.into();
        if name.is_empty() {
            return Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                "ref_type name must be a non-empty string",
                span.cloned(),
            ));
        }
        Ok(Self::Ref { name })
    }

    /// Constructs a decimal, quantity, or money [`PropertyTypeSpec`].
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] if both `unit` and `currency` are set.
    pub fn decimal_type(
        scale: u64,
        unit: Option<UnitRef>,
        currency: Option<String>,
        rounding: Option<Rounding>,
        span: Option<&SourceSpan>,
    ) -> Result<Self, PyAuthoringError> {
        if unit.is_some() && currency.is_some() {
            return Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                "decimal cannot specify both unit and currency",
                span.cloned(),
            ));
        }
        Ok(Self::Decimal {
            scale,
            unit,
            currency,
            per_unit: None,
            rounding,
        })
    }

    /// Constructs a rate [`PropertyTypeSpec`] (`currency` per `per_unit`).
    #[must_use]
    pub fn rate_type(
        scale: u64,
        currency: impl Into<String>,
        per_unit: UnitRef,
        rounding: Option<Rounding>,
    ) -> Self {
        Self::Decimal {
            scale,
            unit: None,
            currency: Some(currency.into()),
            per_unit: Some(per_unit),
            rounding,
        }
    }

    /// Serializes this specification into canonical `EntityIR` v2 [`JsonValue`].
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        match self {
            Self::Text { format } => {
                obj.insert("kind".to_owned(), JsonValue::String("text".to_owned()));
                if let Some(fmt) = format {
                    obj.insert(
                        "format".to_owned(),
                        JsonValue::String(fmt.as_str().to_owned()),
                    );
                }
            }
            Self::Integer => {
                obj.insert("kind".to_owned(), JsonValue::String("integer".to_owned()));
            }
            Self::Boolean => {
                obj.insert("kind".to_owned(), JsonValue::String("boolean".to_owned()));
            }
            Self::Date => {
                obj.insert("kind".to_owned(), JsonValue::String("date".to_owned()));
            }
            Self::Timestamp { timezone } => {
                obj.insert("kind".to_owned(), JsonValue::String("timestamp".to_owned()));
                obj.insert(
                    "timezone".to_owned(),
                    JsonValue::String(timezone.as_str().to_owned()),
                );
            }
            Self::Duration => {
                obj.insert("kind".to_owned(), JsonValue::String("duration".to_owned()));
            }
            Self::Ref { name } => {
                obj.insert("kind".to_owned(), JsonValue::String("ref".to_owned()));
                obj.insert("name".to_owned(), JsonValue::String(name.clone()));
            }
            Self::Decimal {
                scale,
                unit,
                currency,
                per_unit,
                rounding,
            } => write_decimal_ir(
                &mut obj,
                *scale,
                unit.as_ref(),
                currency.as_deref(),
                per_unit.as_ref(),
                *rounding,
            ),
            Self::Enum { values, .. } => {
                obj.insert("kind".to_owned(), JsonValue::String("enum".to_owned()));
                let items = values
                    .iter()
                    .map(|val| {
                        let mut entry = BTreeMap::new();
                        entry.insert("value".to_owned(), JsonValue::String(val.clone()));
                        JsonValue::Object(entry)
                    })
                    .collect();
                obj.insert("values".to_owned(), JsonValue::Array(items));
            }
        }
        JsonValue::Object(obj)
    }
}

fn write_decimal_ir(
    obj: &mut BTreeMap<String, JsonValue>,
    scale: u64,
    unit: Option<&UnitRef>,
    currency: Option<&str>,
    per_unit: Option<&UnitRef>,
    rounding: Option<Rounding>,
) {
    obj.insert("kind".to_owned(), JsonValue::String("decimal".to_owned()));
    obj.insert("scale".to_owned(), JsonValue::Number(scale.to_string()));
    if let Some(u) = unit {
        obj.insert("unit".to_owned(), u.to_ir());
    }
    if let Some(curr) = currency {
        let mut src = BTreeMap::new();
        src.insert("links".to_owned(), JsonValue::Array(Vec::new()));
        src.insert("property".to_owned(), JsonValue::String(curr.to_owned()));
        obj.insert("currencySource".to_owned(), JsonValue::Object(src));
    }
    if let Some(pu) = per_unit {
        obj.insert("perUnit".to_owned(), pu.to_ir());
    }
    if let Some(r) = rounding {
        obj.insert(
            "rounding".to_owned(),
            JsonValue::String(r.as_str().to_owned()),
        );
    }
}

/// Exact property default literal admitted in `EntityIR` v2.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FieldDefault {
    /// String or `StrEnum` member wire value.
    Str(String),
    /// Boolean default.
    Bool(bool),
    /// Fixed-point exact decimal string (validated via [`format_exact_decimal`]).
    Decimal(String),
    /// Exact integer default (serialized as canonical string in `EntityIR` v2).
    Int(String),
    /// Explicit `None` literal (rejected at lowering with [`CODE_SCHEMA`] or [`crate::error::CODE_FLOW_LITERAL`]).
    Null,
}

impl FieldDefault {
    /// Constructs a validated decimal default.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] if `raw` is not an exact bounded decimal.
    pub fn decimal(raw: &str, span: Option<SourceSpan>) -> Result<Self, PyAuthoringError> {
        Ok(Self::Decimal(format_exact_decimal(raw, span)?))
    }

    /// Serializes this default value for an object-type property.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] for unsupported defaults such as `None`.
    pub fn to_ir(
        &self,
        owner_field: &str,
        span: Option<&SourceSpan>,
    ) -> Result<JsonValue, PyAuthoringError> {
        match self {
            Self::Str(s) | Self::Decimal(s) | Self::Int(s) => Ok(JsonValue::String(s.clone())),
            Self::Bool(b) => Ok(JsonValue::Bool(*b)),
            Self::Null => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported default None on {owner_field}"),
                span.cloned(),
            )),
        }
    }
}

/// Optional property description and/or exact default (`FieldMeta(...)`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FieldMeta {
    /// Optional property description.
    pub description: Option<String>,
    /// Optional exact property default.
    pub default: Option<FieldDefault>,
}

/// Typed link specification (`link(Target, ...)`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkSpec {
    /// Target `Concept` dotted name (e.g. `"hr.Employee"`).
    pub target: String,
    /// Target multiplicity (default `"one"`).
    pub to: Multiplicity,
    /// Source multiplicity (default `"many"`).
    pub from_: Multiplicity,
    /// Whether the link is required (default `false`).
    pub required: bool,
    /// Optional inverse link name (object types only).
    pub inverse: Option<String>,
    /// Whether the link graph must be acyclic (object types only).
    pub acyclic: bool,
    /// Optional link description.
    pub description: Option<String>,
}

impl LinkSpec {
    /// Constructs a [`LinkSpec`] with default cardinality (`to="one"`, `from_="many"`).
    #[must_use]
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            to: Multiplicity::One,
            from_: Multiplicity::Many,
            required: false,
            inverse: None,
            acyclic: false,
            description: None,
        }
    }

    /// Serializes this link into `EntityIR` v2 [`JsonValue`], failing closed on interface-incompatible options.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] if `for_interface` is `true`
    /// and `acyclic` or `inverse` is set.
    pub fn to_ir(
        &self,
        link_id: &str,
        for_interface: bool,
        span: Option<&SourceSpan>,
    ) -> Result<JsonValue, PyAuthoringError> {
        if for_interface {
            if self.acyclic {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_SCHEMA,
                    format!(
                        "interface link '{link_id}' cannot declare `acyclic=True` (EntityIR v2 InterfaceLink does not support acyclic)"
                    ),
                    span.cloned(),
                ));
            }
            if let Some(inv) = &self.inverse {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_SCHEMA,
                    format!(
                        "interface link '{link_id}' cannot declare `inverse='{inv}'` (EntityIR v2 InterfaceLink does not support inverse)"
                    ),
                    span.cloned(),
                ));
            }
        }

        let mut card = BTreeMap::new();
        card.insert(
            "from".to_owned(),
            JsonValue::String(self.from_.as_str().to_owned()),
        );
        if self.required {
            card.insert("required".to_owned(), JsonValue::Bool(true));
        }
        card.insert(
            "to".to_owned(),
            JsonValue::String(self.to.as_str().to_owned()),
        );

        let mut base = BTreeMap::new();
        base.insert("cardinality".to_owned(), JsonValue::Object(card));
        if let Some(desc) = &self.description {
            base.insert("description".to_owned(), JsonValue::String(desc.clone()));
        }
        base.insert("id".to_owned(), JsonValue::String(link_id.to_owned()));
        base.insert("target".to_owned(), JsonValue::String(self.target.clone()));
        if !for_interface {
            if self.acyclic {
                base.insert("acyclic".to_owned(), JsonValue::Bool(true));
            }
            if let Some(inv) = &self.inverse {
                base.insert("inverse".to_owned(), JsonValue::String(inv.clone()));
            }
        }
        Ok(JsonValue::Object(base))
    }
}

/// Identity key specification (`KeySpec("primary", ("id",))`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeySpec {
    /// Key name (e.g. `"primary"`).
    pub name: String,
    /// Ordered property identifiers forming the key.
    pub properties: Vec<String>,
}

impl KeySpec {
    /// Serializes this identity key into `EntityIR` v2 [`JsonValue`].
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        obj.insert("name".to_owned(), JsonValue::String(self.name.clone()));
        obj.insert(
            "properties".to_owned(),
            JsonValue::Array(
                self.properties
                    .iter()
                    .map(|p| JsonValue::String(p.clone()))
                    .collect(),
            ),
        );
        JsonValue::Object(obj)
    }
}

/// Closed masking strategy specification for [`MarkingDecl`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MaskSpec {
    /// Replace value with redaction marker (`{"kind": "redacted"}`).
    Redacted,
    /// Replace value with `null` (`{"kind": "null"}`).
    Null,
    /// Refuse disclosure (`{"kind": "refuse"}`).
    Refuse,
    /// Retain only the last `count` characters (`{"count": count, "kind": "last-n"}`).
    LastN(u64),
    /// Deterministic pseudonym scoped to `subject` (`{"kind": "pseudonym", "subject": subject}`).
    Pseudonym(String),
}

impl MaskSpec {
    /// Constructs a pseudonym [`MaskSpec`], rejecting empty `subject` strings.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] if `subject` is empty.
    pub fn pseudonym(
        subject: impl Into<String>,
        span: Option<&SourceSpan>,
    ) -> Result<Self, PyAuthoringError> {
        let subject = subject.into();
        if subject.is_empty() {
            return Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                "mask_pseudonym subject must be a non-empty str",
                span.cloned(),
            ));
        }
        Ok(Self::Pseudonym(subject))
    }

    /// Serializes this masking strategy into canonical [`JsonValue`] IR.
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        match self {
            Self::Redacted => {
                obj.insert("kind".to_owned(), JsonValue::String("redacted".to_owned()));
            }
            Self::Null => {
                obj.insert("kind".to_owned(), JsonValue::String("null".to_owned()));
            }
            Self::Refuse => {
                obj.insert("kind".to_owned(), JsonValue::String("refuse".to_owned()));
            }
            Self::LastN(count) => {
                obj.insert("count".to_owned(), JsonValue::Number(count.to_string()));
                obj.insert("kind".to_owned(), JsonValue::String("last-n".to_owned()));
            }
            Self::Pseudonym(subject) => {
                obj.insert("kind".to_owned(), JsonValue::String("pseudonym".to_owned()));
                obj.insert("subject".to_owned(), JsonValue::String(subject.clone()));
            }
        }
        JsonValue::Object(obj)
    }
}

/// Single classification marking entry inside a [`MarkingsCatalog`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MarkingDecl {
    /// Marking identifier (e.g. `"pii.demographic"`).
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Sensitivity severity.
    pub severity: MarkingSeverity,
    /// Category string (e.g. `"pii"`).
    pub category: String,
    /// Closed masking policy.
    pub mask: MaskSpec,
}

impl MarkingDecl {
    /// Serializes this marking declaration into canonical [`JsonValue`] IR.
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        obj.insert(
            "category".to_owned(),
            JsonValue::String(self.category.clone()),
        );
        obj.insert("id".to_owned(), JsonValue::String(self.id.clone()));
        obj.insert("label".to_owned(), JsonValue::String(self.label.clone()));
        obj.insert("mask".to_owned(), self.mask.to_ir());
        obj.insert(
            "severity".to_owned(),
            JsonValue::String(self.severity.as_str().to_owned()),
        );
        JsonValue::Object(obj)
    }
}

/// Classification markings catalog lowering to an `EntityIR` v2 artifact envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkingsCatalog {
    /// Catalog artifact identifier (e.g. `"hr.privacy.markings"`).
    pub id: String,
    /// Human-readable catalog title.
    pub title: String,
    /// Semantic version string.
    pub version: String,
    /// Ordered marking declarations.
    pub markings: Vec<MarkingDecl>,
}

impl MarkingsCatalog {
    /// Lowers this catalog into an `irVersion: "0.1"` `EntityIR` v2 artifact envelope.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] if canonical envelope construction fails.
    pub fn lower(&self, options: &EnvelopeOptions) -> Result<JsonValue, PyAuthoringError> {
        let namespace = self.id.split('.').next().unwrap_or("");
        let mut payload = BTreeMap::new();
        payload.insert(
            "entityVersion".to_owned(),
            JsonValue::String("2.0".to_owned()),
        );
        payload.insert(
            "markings".to_owned(),
            JsonValue::Array(self.markings.iter().map(MarkingDecl::to_ir).collect()),
        );
        let header = EnvelopeHeader {
            kind: "entity",
            artifact_id: &self.id,
            namespace,
            name: &self.title,
            version: &self.version,
        };
        build_envelope(&header, payload, options, &[])
    }
}

/// External worker handle declaration (`fn_ref(...)`, outside the artifact stream).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FnRef {
    /// Worker identifier (e.g. `"hr.solver.ortools_roster"`).
    pub id: String,
    /// Semantic version string.
    pub version: String,
    /// Input type name.
    pub input_type: String,
    /// Output type name.
    pub output_type: String,
}
