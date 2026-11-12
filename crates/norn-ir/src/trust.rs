//! VIEW-TRUST-1: how far a definition's output can be trusted without
//! running it.
//!
//! Levels order strongest first: `Native < Proven < ContractChecked <
//! Opaque` under [`Ord`], so composing definitions takes the maximum, the
//! weakest level of its parts ([`TrustLevel::compose`]).
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// VIEW-TRUST-1: how far a definition's output can be trusted without running it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Computed by Norn from declared semantics on any qualified engine (exact or DuckDB).
    Native,
    /// Native and covered by a machine-checked proof.
    Proven,
    /// Opaque internals whose outputs are checked against declared contracts before they are written.
    ContractChecked,
    /// Opaque internals; outputs are labelled and not trusted beyond their type.
    Opaque,
}

impl TrustLevel {
    /// The trust of a definition built from `self` and `other`: the weaker of
    /// the two.
    #[must_use]
    pub fn compose(self, other: Self) -> Self {
        self.max(other)
    }

    /// The wire name (`native`, `proven`, `contract_checked`, `opaque`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Proven => "proven",
            Self::ContractChecked => "contract_checked",
            Self::Opaque => "opaque",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TrustLevel;

    #[test]
    fn composition_takes_the_weakest_level() {
        use TrustLevel::{ContractChecked, Native, Opaque, Proven};
        let all = [Native, Proven, ContractChecked, Opaque];
        for (index, level) in all.iter().enumerate() {
            for (other_index, other) in all.iter().enumerate() {
                let weakest = all.get(index.max(other_index)).copied();
                assert_eq!(Some(level.compose(*other)), weakest);
            }
        }
    }

    #[test]
    fn wire_names_are_snake_case() {
        for level in [
            TrustLevel::Native,
            TrustLevel::Proven,
            TrustLevel::ContractChecked,
            TrustLevel::Opaque,
        ] {
            assert_eq!(
                serde_json::to_value(level).ok(),
                Some(serde_json::Value::String(level.as_str().to_owned()))
            );
        }
    }
}
