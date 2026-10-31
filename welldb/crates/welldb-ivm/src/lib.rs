//! Exact arithmetic primitives for incremental metrics.
//! This slice has no live-view evaluator or export adapter yet.
#![forbid(unsafe_code)]

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

