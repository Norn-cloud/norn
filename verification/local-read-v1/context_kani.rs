//! Bounded READ-LOCAL-1 acceptance semantics using the production predicate.
#[path = "../../crates/norn-kernel/src/read_token_check.rs"]
mod read_token_check;

#[kani::proof]
fn context_and_exactness_are_both_required() {
    let matches: bool = kani::any();
    let exact: bool = kani::any();
    let complete: bool = kani::any();
    let require_exact: bool = kani::any();
    let accepted = read_token_check::accepts(matches, exact && complete, require_exact);
    assert!(!accepted || matches);
    assert!(!accepted || !require_exact || exact && complete);
    assert_eq!(accepted, matches && (!require_exact || exact && complete));
}
