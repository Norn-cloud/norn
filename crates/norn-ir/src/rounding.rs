//! Explicit quantization modes shared by exact value and computation contracts.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// FOLD-ARITHMETIC / VAL-ROUND: a rounding site declares its mode explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    Refuse,
    TowardZero,
    HalfEven,
    HalfAwayFromZero,
}

impl Rounding {
    /// VAL-ROUND: omitted modes are exact/refuse before canonicalization.
    #[must_use]
    pub const fn effective(mode: Option<Self>) -> Self {
        match mode {
            Some(mode) => mode,
            None => Self::Refuse,
        }
    }

    /// Canonical wire spelling; the default Refuse field is omitted.
    #[must_use]
    pub const fn canonical_field(self) -> Option<&'static str> {
        match self {
            Self::Refuse => None,
            Self::TowardZero => Some("toward_zero"),
            Self::HalfEven => Some("half_even"),
            Self::HalfAwayFromZero => Some("half_away_from_zero"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rounding;

    #[test]
    fn omitted_and_explicit_refuse_have_one_canonical_mode() {
        let all = [
            Rounding::Refuse,
            Rounding::TowardZero,
            Rounding::HalfEven,
            Rounding::HalfAwayFromZero,
        ];
        assert_eq!(
            Rounding::effective(None),
            Rounding::effective(Some(Rounding::Refuse))
        );
        for mode in all {
            assert_eq!(Rounding::effective(Some(mode)), mode);
            match mode {
                Rounding::Refuse => assert!(mode.canonical_field().is_none()),
                Rounding::TowardZero | Rounding::HalfEven | Rounding::HalfAwayFromZero => {
                    assert!(mode.canonical_field().is_some());
                }
            }
        }
    }
}
