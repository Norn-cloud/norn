//! Bounded production ownership predicate for TXN-CELL-1.
#[path = "../../crates/norn-kernel/src/profile_ownership.rs"]
mod profile_ownership;
use profile_ownership::{AtomicOwnership, supports_local};

#[kani::proof]
fn unknown_and_foreign_never_support_local_execution() {
    let foreign: bool = kani::any();
    let unresolved = if foreign {
        AtomicOwnership::Foreign
    } else {
        AtomicOwnership::Unknown
    };
    assert!(!supports_local(unresolved));
    assert!(supports_local(AtomicOwnership::Local));
}
