//! Explicit environment-format selection for retained receipts.

/// VAL-ENV: supported formats are distinct domains; unknown versions refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeEnvVersion {
    /// Historical projection without currency-source bindings.
    V1,
    /// Currency-source-aware projection for new admissions.
    V2,
}

impl TypeEnvVersion {
    /// Decode a receipt's version without reinterpreting unknown formats.
    #[must_use]
    pub const fn from_number(version: u32) -> Option<Self> {
        match version {
            1 => Some(Self::V1),
            2 => Some(Self::V2),
            _ => None,
        }
    }

    /// The exact version written in the canonical preimage and reference.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}
