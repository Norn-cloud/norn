//! Kani checks the production version successor, not a handwritten copy.
#[path = "../../../crates/norn-kernel/src/version_check.rs"]
mod version_check;

#[kani::proof]
fn successor_never_wraps() {
    let version: u64 = kani::any();
    if let Some(next) = version_check::checked_next_version(version) {
        assert!(next > version);
    }
}

#[kani::proof]
fn exhaustion_is_exact() {
    let version: u64 = kani::any();
    assert_eq!(version_check::checked_next_version(version).is_none(), version == u64::MAX);
}

#[kani::proof]
fn successor_has_no_gaps() {
    let version: u64 = kani::any();
    if let Some(next) = version_check::checked_next_version(version) {
        assert_eq!(next.checked_sub(1), Some(version));
    }
}
