//! OUTPUT-ATOMIC: finite selection kernel, separate from SQL durability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Building,
    Ready,
    Approved,
    Active,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Selection {
    Previous,
    Candidate,
}
/// Approval never selects a candidate; only activation can expose it.
pub(super) const fn select(phase: Phase, previous: bool, candidate: bool) -> Option<Selection> {
    match phase {
        Phase::Active if candidate => Some(Selection::Candidate),
        Phase::Building | Phase::Ready | Phase::Approved if previous => Some(Selection::Previous),
        Phase::Building | Phase::Ready | Phase::Approved | Phase::Active => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn only_activation_exposes_a_present_candidate(phase in 0u8..4,previous in proptest::bool::ANY,candidate in proptest::bool::ANY) {
            let phase = match phase {0=>Phase::Building,1=>Phase::Ready,2=>Phase::Approved,_=>Phase::Active};
            let chosen=select(phase,previous,candidate);
            proptest::prop_assert_eq!(chosen==Some(Selection::Candidate),phase==Phase::Active && candidate);
            proptest::prop_assert_eq!(chosen==Some(Selection::Previous),phase!=Phase::Active && previous);
        }
    }
}
