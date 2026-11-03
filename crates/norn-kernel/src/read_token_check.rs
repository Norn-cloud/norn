//! READ-LOCAL-1: context equality is necessary even when stale reads are allowed.

pub(crate) const fn accepts(
    context_matches: bool,
    exact_complete: bool,
    requires_exact: bool,
) -> bool {
    context_matches && (!requires_exact || exact_complete)
}
