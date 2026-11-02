//! CELL-ID-1: logical cell identity is independent of business scope and host.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An opaque 128-bit cell identity, encoded as exactly 32 lowercase hex digits.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String", regex(pattern = "^[0-9a-f]{32}$"))]
pub struct CellId(String);

impl TryFrom<String> for CellId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 32
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Ok(Self(value))
        } else {
            Err("cell identity must be 32 lowercase hexadecimal digits")
        }
    }
}

impl From<CellId> for String {
    fn from(value: CellId) -> Self {
        value.0
    }
}

impl CellId {
    /// Canonical persisted representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::CellId;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn canonical_cell_ids_roundtrip(value in "[0-9a-f]{32}") {
            let id = CellId::try_from(value.clone()).unwrap();
            prop_assert_eq!(id.as_str(), value.as_str());
            let encoded = serde_json::to_string(&id).unwrap();
            prop_assert_eq!(serde_json::from_str::<CellId>(&encoded).unwrap(), id);
        }

        #[test]
        fn noncanonical_identity_refuses(value in ".*") {
            let expected = value.len() == 32 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            prop_assert_eq!(CellId::try_from(value).is_ok(), expected);
        }
    }
}
