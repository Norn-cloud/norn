//! QUERY-FRESH-1: root-query eligibility cannot stand in for a membership fence.

/// Why a governed read cannot be attached to the prepared action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernedReadRefusal {
    /// The producer is not exact cell evaluation with complete coverage.
    StaleInput,
    /// Current query plans read roots without Lane A's captured membership proof.
    UnfencedRoot,
    /// The existing action proof or an added point version no longer agrees.
    ConflictingReadSet,
}
impl GovernedReadRefusal {
    /// Stable QUERY-FRESH-1 diagnostic, including unavailable dependency fences.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::StaleInput | Self::UnfencedRoot | Self::ConflictingReadSet => {
                "core.compute.stale_input"
            }
        }
    }
}
impl std::fmt::Display for GovernedReadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::StaleInput => "governed use requires exact complete cell inputs",
            Self::UnfencedRoot => "root membership has no captured commit fence",
            Self::ConflictingReadSet => "prepared action read guards disagree",
        };
        write!(f, "{}: {reason}", self.code())
    }
}
impl std::error::Error for GovernedReadRefusal {}

/// QUERY-FRESH-1: even eligible root inputs lack the required membership proof.
#[must_use]
pub const fn root_refusal(cell: bool, exact: bool, complete: bool) -> GovernedReadRefusal {
    if cell && exact && complete {
        GovernedReadRefusal::UnfencedRoot
    } else {
        GovernedReadRefusal::StaleInput
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn governed_root_never_grants_authority(cell in proptest::bool::ANY, exact in proptest::bool::ANY, complete in proptest::bool::ANY) {
            let refusal = root_refusal(cell, exact, complete);
            proptest::prop_assert_eq!(refusal.code(), "core.compute.stale_input");
            proptest::prop_assert_eq!(refusal == GovernedReadRefusal::UnfencedRoot, cell && exact && complete);
        }
    }
}
