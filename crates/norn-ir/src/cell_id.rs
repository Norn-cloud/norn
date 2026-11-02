//! CELL-ID-1: logical cell identity is independent of business scope and host.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An opaque 128-bit cell identity, encoded as exactly 32 lowercase hex digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CellId(String);

impl JsonSchema for CellId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CellId".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "pattern": "^[0-9a-f]{32}$" })
    }
}

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
#[allow(clippy::unwrap_used, reason = "property assertions")]
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
        fn noncanonical_identity_refuses(value in prop_oneof![
            "[A-Zg-z][0-9a-f]{31}", "[0-9a-f]{0,31}", "[0-9a-f]{33,64}"
        ]) {
            prop_assert!(CellId::try_from(value.clone()).is_err());
            prop_assert!(serde_json::from_value::<CellId>(serde_json::json!(value)).is_err());
        }
    }
}
