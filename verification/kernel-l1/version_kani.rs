//! Kani checks the production version successor, not a handwritten copy.
#[path = "../../crates/norn-kernel/src/version_check.rs"]
mod version_check;

#[kani::proof]
fn id_stable_genesis_and_live_rows_are_never_deleted() {
    let version: u64 = kani::any();
    assert!(!version_check::has_committed_deletion(version, true));
    assert!(!version_check::has_committed_deletion(0, kani::any()));
}

#[kani::proof]
fn id_stable_retained_absence_is_a_deletion() {
    let version: u64 = kani::any();
    kani::assume(version > 0);
    assert!(version_check::has_committed_deletion(version, false));
    if let Some(next) = version_check::checked_next_version(version) {
        assert!(next > version);
        assert!(!version_check::has_committed_deletion(next, true));
    }
}

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
    assert_eq!(
        version_check::checked_next_version(version).is_none(),
        version == u64::MAX
    );
}

#[kani::proof]
fn successor_has_no_gaps() {
    let version: u64 = kani::any();
    if let Some(next) = version_check::checked_next_version(version) {
        assert_eq!(next.checked_sub(1), Some(version));
    }
}
