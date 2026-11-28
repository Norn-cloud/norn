//! Closed schema enums, unit references, and dotted object-type name validation
//! matching `tools/norn-py/norn_py/schema.py`.

use std::collections::BTreeMap;

use crate::canonical::JsonValue;
use crate::error::{CODE_SCHEMA, PyAuthoringError, SourceSpan};

pub use crate::schema::types_spec::{
    FieldDefault, FieldMeta, FnRef, KeySpec, LinkSpec, MarkingDecl, MarkingsCatalog, MaskSpec,
    PropertyTypeSpec,
};

/// Admitted unit-system namespace for [`UnitRef`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnitSystem {
    /// Unified Code for Units of Measure (`"ucum"`).
    Ucum,
    /// Quantities, Units, Dimensions and Data Types (`"qudt"`).
    Qudt,
}

impl UnitSystem {
    /// Parses a unit system literal (`"ucum"` or `"qudt"`).
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on any other value.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "ucum" => Ok(Self::Ucum),
            "qudt" => Ok(Self::Qudt),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported unit system '{raw}' (expected 'ucum' or 'qudt')"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ucum => "ucum",
            Self::Qudt => "qudt",
        }
    }
}

/// Exact unit reference (`UnitRef(system="ucum", code="h")`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnitRef {
    /// Unit system (`"ucum"` or `"qudt"`).
    pub system: UnitSystem,
    /// Unit code within `system` (e.g. `"h"`, `"d"`).
    pub code: String,
}

impl UnitRef {
    /// Constructs a new [`UnitRef`].
    #[must_use]
    pub fn new(system: UnitSystem, code: impl Into<String>) -> Self {
        Self {
            system,
            code: code.into(),
        }
    }

    /// Serializes this [`UnitRef`] to canonical [`JsonValue`] IR.
    #[must_use]
    pub fn to_ir(&self) -> JsonValue {
        let mut obj = BTreeMap::new();
        obj.insert("code".to_owned(), JsonValue::String(self.code.clone()));
        obj.insert(
            "system".to_owned(),
            JsonValue::String(self.system.as_str().to_owned()),
        );
        JsonValue::Object(obj)
    }
}

/// Decimal rounding policy admitted in `EntityIR` v2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rounding {
    /// Refuse inexact results (`"refuse"`).
    Refuse,
    /// Round toward zero (`"toward_zero"`).
    TowardZero,
    /// Round half to even (`"half_even"`).
    HalfEven,
    /// Round half away from zero (`"half_away_from_zero"`).
    HalfAwayFromZero,
}

impl Rounding {
    /// Parses a rounding policy string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported rounding modes.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "refuse" => Ok(Self::Refuse),
            "toward_zero" => Ok(Self::TowardZero),
            "half_even" => Ok(Self::HalfEven),
            "half_away_from_zero" => Ok(Self::HalfAwayFromZero),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported rounding mode '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::TowardZero => "toward_zero",
            Self::HalfEven => "half_even",
            Self::HalfAwayFromZero => "half_away_from_zero",
        }
    }
}

/// Optional text format qualifier for `text(...)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextFormat {
    /// Email format (`"email"`).
    Email,
    /// `URI` format (`"uri"`).
    Uri,
    /// `UUID` format (`"uuid"`).
    Uuid,
}

impl TextFormat {
    /// Parses a text format string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported formats.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "email" => Ok(Self::Email),
            "uri" => Ok(Self::Uri),
            "uuid" => Ok(Self::Uuid),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported text format '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Uri => "uri",
            Self::Uuid => "uuid",
        }
    }
}

/// Timestamp timezone policy for `timestamp_type(...)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TimezonePolicy {
    /// `UTC` instant (`"utc"`).
    #[default]
    Utc,
    /// Explicit numeric offset (`"offset"`).
    Offset,
}

impl TimezonePolicy {
    /// Parses a timezone policy string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported policies.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "utc" => Ok(Self::Utc),
            "offset" => Ok(Self::Offset),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported timezone policy '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utc => "utc",
            Self::Offset => "offset",
        }
    }
}

/// Link endpoint multiplicity (`"one"` or `"many"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Multiplicity {
    /// Single target (`"one"`).
    One,
    /// Unbounded collection (`"many"`).
    Many,
}

impl Multiplicity {
    /// Parses a multiplicity string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported values.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "one" => Ok(Self::One),
            "many" => Ok(Self::Many),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported multiplicity '{raw}' (expected 'one' or 'many')"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Many => "many",
        }
    }
}

/// Semantic role of an `EntityIR` v2 object type or interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoleKind {
    /// Endurant entity (`"thing"`).
    Thing,
    /// Perdurant event or interval (`"happening"`).
    Happening,
}

impl RoleKind {
    /// Parses a role kind string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported values.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "thing" => Ok(Self::Thing),
            "happening" => Ok(Self::Happening),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported role '{raw}' (expected 'thing' or 'happening')"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Thing => "thing",
            Self::Happening => "happening",
        }
    }
}

/// Severity level for a [`MarkingDecl`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MarkingSeverity {
    /// Low sensitivity (`"low"`).
    Low,
    /// Moderate sensitivity (`"moderate"`).
    Moderate,
    /// High sensitivity (`"high"`).
    High,
    /// Critical sensitivity (`"critical"`).
    Critical,
}

impl MarkingSeverity {
    /// Parses a marking severity string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported values.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "low" => Ok(Self::Low),
            "moderate" => Ok(Self::Moderate),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported marking severity '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Moderate => "moderate",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Classification of a per-record `@invariant`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum InvariantClassification {
    /// Hard integrity invariant (default; omitted from wire IR).
    #[default]
    Integrity,
    /// Advisory data-quality invariant (`"quality"`).
    Quality,
}

impl InvariantClassification {
    /// Parses an invariant classification string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported values.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "integrity" => Ok(Self::Integrity),
            "quality" => Ok(Self::Quality),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported invariant classification '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Integrity => "integrity",
            Self::Quality => "quality",
        }
    }
}

/// Severity of a `LawIR` declaration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LawSeverity {
    /// Blocking law violation (`"error"`).
    #[default]
    Error,
    /// Advisory warning (`"warning"`).
    Warning,
}

impl LawSeverity {
    /// Parses a law severity string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported values.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "error" => Ok(Self::Error),
            "warning" => Ok(Self::Warning),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported law severity '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// Temporal modality of a `LawIR` declaration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LawModality {
    /// Must always hold (`"always"`).
    #[default]
    Always,
    /// Must never hold (`"never"`).
    Never,
    /// Must eventually hold (`"eventually"`).
    Eventually,
}

impl LawModality {
    /// Parses a law modality string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported values.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            "eventually" => Ok(Self::Eventually),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported law modality '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::Eventually => "eventually",
        }
    }
}

/// Root scope kind for a `LawIR` declaration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScopeRootKind {
    /// Company-wide law (`"company"`).
    Company,
    /// Application-wide law (`"app"`).
    App,
    /// Cell-scoped law (`"cell"`).
    Cell,
    /// Entity-scoped law (`"entity"`).
    #[default]
    Entity,
}

impl ScopeRootKind {
    /// Parses a scope root kind string.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_SCHEMA`] on unsupported values.
    pub fn parse(raw: &str, span: Option<&SourceSpan>) -> Result<Self, PyAuthoringError> {
        match raw {
            "company" => Ok(Self::Company),
            "app" => Ok(Self::App),
            "cell" => Ok(Self::Cell),
            "entity" => Ok(Self::Entity),
            _ => Err(PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                format!("unsupported scope_root_kind '{raw}'"),
                span.cloned(),
            )),
        }
    }

    /// Returns the wire identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::App => "app",
            Self::Cell => "cell",
            Self::Entity => "entity",
        }
    }
}

/// Validates a dotted `EntityIR` v2 type name against
/// `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*\.[A-Z][A-Za-z0-9]*$`.
#[must_use]
pub fn is_valid_object_type_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let rest: Vec<&str> = parts.collect();
    let Some((&last, middle)) = rest.split_last() else {
        return false;
    };
    if !is_lower_ns_segment(first) {
        return false;
    }
    for &seg in middle {
        if !is_lower_ns_segment(seg) {
            return false;
        }
    }
    is_pascal_type_segment(last)
}

fn is_lower_ns_segment(seg: &str) -> bool {
    let mut bytes = seg.bytes();
    let Some(head) = bytes.next() else {
        return false;
    };
    head.is_ascii_lowercase()
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_pascal_type_segment(seg: &str) -> bool {
    let mut bytes = seg.bytes();
    let Some(head) = bytes.next() else {
        return false;
    };
    head.is_ascii_uppercase() && bytes.all(|b| b.is_ascii_alphanumeric())
}
