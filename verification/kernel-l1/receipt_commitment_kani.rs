//! SNAPSHOT-RECEIPT-1: bounded digest equality, never unbound acceptance.
#[path = "../../crates/norn-cell/src/receipt_commitment.rs"]
mod commitment;
#[kani::proof]
fn only_the_exact_bound_digest_accepts() {
    let a: [u8; 2] = kani::any(); let b: [u8; 2] = kani::any();
    if let (Ok(a), Ok(b)) = (core::str::from_utf8(&a), core::str::from_utf8(&b)) {
        assert!(!commitment::receipt_digest_matches(None, a));
        assert!(commitment::receipt_digest_matches(Some(a), a));
        assert_eq!(commitment::receipt_digest_matches(Some(a), b), a == b);
    }
}
