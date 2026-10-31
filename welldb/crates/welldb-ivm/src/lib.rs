//! Exact decimal primitives for incremental metrics.
//! This slice has no live-view evaluator or export adapter yet.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

/// Verified export commit sequence.
pub type Seq = u64;
/// Verified export content address, supplied by the future adapter.
pub type RecordHash = String;

/// Norn-compatible normalized exact decimal: |mantissa| < 2^96, scale ≤ 28.
/// Fields are private so invalid decimals cannot enter through construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "serde",
    serde(try_from = "DecimalWire", into = "DecimalWire")
)]
pub struct Decimal {
    mantissa: i128,
    scale: u8,
}
impl Decimal {
    /// Normalize trailing fractional zeros, then check exact representability.
    /// # Errors
    /// Returns `AggregateOverflow` for an unrepresentable decimal.
    pub fn new(mut mantissa: i128, mut scale: u8) -> Result<Self, MetricError> {
        while scale > 0 && mantissa.checked_rem(10) == Some(0) {
            mantissa = mantissa
                .checked_div(10)
                .ok_or(MetricError::AggregateOverflow)?;
            scale = scale.checked_sub(1).ok_or(MetricError::AggregateOverflow)?;
        }
        if scale > 28 || mantissa.unsigned_abs() >= (1_u128 << 96) {
            return Err(MetricError::AggregateOverflow);
        }
        Ok(Self { mantissa, scale })
    }
    /// Normalized signed coefficient.
    #[must_use]
    pub const fn mantissa(self) -> i128 {
        self.mantissa
    }
    /// Normalized fractional digit count.
    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }
}

/// Whole-result refusals, in canonical capture/execution precedence (§24.1–4).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MetricError {
    /// Invalid plan admission.
    Admission,
    /// Unsupported profile or authorization context.
    Unsupported,
    /// Missing/multiple link or physically absent target (`metric.snapshot_link`).
    SnapshotLink,
    /// Masked captured operand (`metric.marking_denied`).
    MarkingDenied,
    /// Captured property violates its declared runtime constraint.
    InvalidField,
    /// Currency differs from the singleton.
    MixedCurrency,
    /// Exact normalized aggregate is outside the Norn decimal domain.
    AggregateOverflow,
    /// Bucket below the disclosure minimum.
    Disclosure,
    /// Population reached `u64::MAX`; never wrap.
    PopulationOverflow,
    /// Epoch, definition or delivery continuity invalidated this backend.
    Rebuilding,
    /// Unknown actor/epoch pair; fail closed.
    UnknownClass,
    /// Native-only N1–N3 refusal (classifier only, never canonical execution).
    DuckDb,
}

/// Exact checked arithmetic.
pub mod arithmetic;
pub use arithmetic::{BucketAgg, ExactSum, PartsError};

// Wire decoding cannot bypass decimal normalization/representability.
#[cfg(feature = "serde")]
#[derive(serde::Serialize, serde::Deserialize)]
struct DecimalWire {
    mantissa: i128,
    scale: u8,
}
#[cfg(feature = "serde")]
impl From<Decimal> for DecimalWire {
    fn from(value: Decimal) -> Self {
        Self {
            mantissa: value.mantissa,
            scale: value.scale,
        }
    }
}
#[cfg(feature = "serde")]
impl TryFrom<DecimalWire> for Decimal {
    type Error = &'static str;
    fn try_from(wire: DecimalWire) -> Result<Self, Self::Error> {
        Self::new(wire.mantissa, wire.scale).map_err(|_error| "unrepresentable decimal")
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let scale = self.scale.max(other.scale);
        // Valid decimals always fit the 256-bit rescaling domain.
        match (
            ExactSum::scaled(*self, scale),
            ExactSum::scaled(*other, scale),
        ) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            (Err(_), _) | (_, Err(_)) => unreachable!("valid Decimal rescaling fits 256 bits"),
        }
    }
}

/// WellDB-owned subset of runtime values; malformed inputs remain representable
/// so validity refusals can be evaluated in capture order, before selection.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Value {
    /// JSON null (invalid for admitted operands).
    Null,
    /// Boolean (invalid for admitted operands).
    Bool(bool),
    /// Exact numeric token.
    Decimal(Decimal),
    /// Text, enum, ISO date or UTC timestamp token.
    Text(String),
}
/// Stable qualified identity, ordered by type then object ID.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ObjectKey {
    /// Concrete object type.
    pub object_type: String,
    /// Object ID; capture order within the measure type.
    pub id: String,
}
/// Exactly one actor at one authorization epoch (§24.4).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassId {
    /// Stable actor identity, scoped by the future adapter.
    pub actor: String,
    /// Authorization epoch.
    pub epoch: u64,
}
/// Per-class authorization capabilities. Context-dependent gates are unsupported.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClassSpec {
    /// Actor/epoch pair.
    pub id: ClassId,
    /// Unsupported if true.
    pub requires_acted_by_context: bool,
    /// Unsupported if true.
    pub requires_link_context: bool,
}
/// Access to one object for one class. Absent access means hidden.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Access {
    /// Object admitted by the per-record policy.
    pub visible: bool,
    /// Operand properties whose markings deny access.
    pub masked: BTreeSet<String>,
}
/// Current object state, retained even when hidden (§24.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Record {
    /// Complete values after adapter-resolved defaults and lifecycle changes.
    pub values: BTreeMap<String, Value>,
    /// Class-specific visibility and markings.
    pub access: BTreeMap<ClassId, Access>,
    /// Last write's export sequence.
    pub seq: Seq,
    /// Last write's export content address.
    pub hash: RecordHash,
}
/// Operand placement; all target operands use the plan's one required link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Placement {
    /// Direct measure property.
    Measure,
    /// Property on the required single forward-link target.
    Target,
}
/// Admitted property constraint; non-null, non-computed operands only.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Property {
    /// Unformatted text.
    Text,
    /// Closed enum values.
    Enum(BTreeSet<String>),
    /// ISO Gregorian date, year 0000..9999.
    Date,
    /// UTC timestamp at nanosecond precision, year 0000..9999.
    Timestamp,
    /// Decimal property maximum fractional scale; no SQL-style precision.
    Decimal {
        /// Maximum fractional digits.
        scale: u8,
    },
    /// Conjoined effective constraints, including inherited/reference-chain sets.
    Constrained {
        /// The underlying admitted scalar property.
        base: Box<Property>,
        /// Every effective constraint set; all must pass.
        constraints: Vec<ScalarConstraints>,
    },
}
/// Effective scalar constraints mirrored from the source property checker.
/// Bounds are exact decimal literals, not bounded runtime Decimals: a declared
/// bound may lie outside the 96-bit runtime domain. Text lengths count Unicode
/// scalar values; patterns use native regex-lite search (unanchored) semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ScalarConstraints {
    /// Inclusive exact lower bound, using the native decimal-literal grammar.
    pub min: Option<String>,
    /// Inclusive exact upper bound.
    pub max: Option<String>,
    /// Minimum number of Unicode scalar values (Text only).
    pub min_length: Option<u64>,
    /// Maximum number of Unicode scalar values (Text only).
    pub max_length: Option<u64>,
    /// Regex-lite pattern that must have a match (Text only).
    pub pattern: Option<String>,
}
impl Property {
    /// Base scalar kind, without effective constraint wrappers.
    #[must_use]
    pub fn base(&self) -> &Self {
        match self {
            Self::Constrained { base, .. } => base.base(),
            Self::Text | Self::Enum(_) | Self::Date | Self::Timestamp | Self::Decimal { .. } => {
                self
            }
        }
    }
}
/// Ordered, deduplicated field binding (§24.1).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Field {
    /// Property identity.
    pub property: String,
    /// Direct or target binding.
    pub placement: Placement,
    /// Runtime value constraint.
    pub kind: Property,
}
/// One required forward link across all lowered fields.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LinkSpec {
    /// Forward link role.
    pub role: String,
    /// Concrete target type.
    pub target_type: String,
}
/// Direct text predicate.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Filter {
    /// Index in the ordered fields.
    pub field: usize,
    /// True for equality; false for inequality.
    pub equal: bool,
    /// Text literal.
    pub value: String,
}
/// Closed count or fixed-scale exact sum.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Aggregate {
    /// Count contributing objects.
    Count,
    /// Sum a direct decimal, at fixed scale ≤ 28.
    Sum {
        /// Amount field index.
        amount: usize,
        /// Fixed accumulator scale.
        scale: u8,
        /// Optional singleton currency field and value.
        currency: Option<(usize, String)>,
    },
}
/// Admitted closed metric profile, mirrored rather than linked to Norn (§24.1).
/// `validate` rejects structural violations; the adapter owns full IR admission.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Plan {
    /// Concrete measure type.
    pub measure_type: String,
    /// One link, present iff any lowered field uses its target.
    pub link: Option<LinkSpec>,
    /// Capture order: time, optional group, amount/currency, then filters,
    /// deduplicating repeated paths by first occurrence.
    pub fields: Vec<Field>,
    /// Time field index.
    pub time: usize,
    /// Optional group field index (at most one).
    pub group: Option<usize>,
    /// Count or exact sum.
    pub aggregate: Aggregate,
    /// At most sixteen direct equality/inequality predicates.
    pub filters: Vec<Filter>,
    /// Fixed half-open window, Unix nanoseconds. Bounds exceed i64 if needed.
    pub window: (i128, i128),
    /// Disclosure minimum, including empty ungrouped populations.
    pub min_group_size: u64,
    /// Pinned installed definition version.
    pub definition_version: u64,
}
/// Unsupported plan/snapshot reason at build time.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Unsupported(pub String);

mod plan;

mod state;
pub use state::*;

mod source;
pub use source::SourceState;

/// Pure metric evaluation semantics.
pub mod semantics;
pub use semantics::{Captured, Contribution, RefusalIndex, RowRefusal, finalize};
