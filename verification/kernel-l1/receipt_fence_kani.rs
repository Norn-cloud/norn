//! Verify the production receipt fence, excluding SQL/host authentication.
#[path = "../../crates/norn-cell/src/receipt_fence.rs"]
mod receipt_fence;

#[kani::proof]
fn consumed_and_changed_frontiers_refuse() {
    let consumed: bool = kani::any();
    let authenticated: u64 = kani::any();
    let current: u64 = kani::any();
    let usable = receipt_fence::usable(consumed, authenticated, current);
    assert_eq!(usable, !consumed && authenticated == current);
    assert!(!receipt_fence::usable(true, authenticated, authenticated));
}
