//! Closed lifecycle and reversal-target states for quantity admission.

use serde::{Deserialize, Serialize};

use super::{QuantityError, QuantityPosting};

/// Closed lifecycle status of a quantity reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    /// Reservation holds quantity and may still be posted, voided, or expired.
    Open,
    /// Reservation was completely posted (terminal).
    Posted,
    /// Reservation was completely voided (terminal).
    Void,
    /// Reservation reached its deadline and expired (terminal).
    Expired,
}

impl ReservationStatus {
    /// Canonical wire and storage name of this reservation status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Posted => "posted",
            Self::Void => "void",
            Self::Expired => "expired",
        }
    }

    /// Whether this reservation is still open and resolvable.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open)
    }
}

impl std::fmt::Display for ReservationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for ReservationStatus {
    type Error = QuantityError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "open" => Ok(Self::Open),
            "posted" => Ok(Self::Posted),
            "void" => Ok(Self::Void),
            "expired" => Ok(Self::Expired),
            _ => Err(QuantityError::Corrupt),
        }
    }
}

impl std::str::FromStr for ReservationStatus {
    type Err = QuantityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s)
    }
}

impl<'de> Deserialize<'de> for ReservationStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ReservationStatusVisitor;

        impl serde::de::Visitor<'_> for ReservationStatusVisitor {
            type Value = ReservationStatus;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(
                    "a reservation status string (\"open\", \"posted\", \"void\", or \"expired\")",
                )
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                ReservationStatus::try_from(value)
                    .map_err(|_| E::unknown_variant(value, &["open", "posted", "void", "expired"]))
            }
        }

        deserializer.deserialize_str(ReservationStatusVisitor)
    }
}

/// Reversal-target state of an original posting.
///
/// Eliminates the impossible state where an original posting is absent yet
/// marked already reversed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OriginalPostingState {
    /// No original posting loaded (or none exists).
    #[default]
    Absent,
    /// Original posting exists and has not yet been reversed.
    Active(QuantityPosting),
    /// Original posting exists and already has a linked reversal.
    AlreadyReversed(QuantityPosting),
}
