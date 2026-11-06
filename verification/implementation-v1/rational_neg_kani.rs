//! Correspondence for the production exact-rational integer negation path.
//!
//! Domain: every i64 numerator, canonical denominator 1 (all integer rationals).
//! Spec: None iff -n is outside i64; otherwise Some((-n, 1)).
//! There is no input-width restriction or sampled corpus. normalize's Euclidean
//! loop takes at most two iterations for denominator 1; Kani checks unwinding
//! assertions, so the unwind limit cannot silently truncate a path.
//! Residual: non-integer rationals, other operations, callers and lowering.
//! This standalone module excludes cfg(test) fixtures and the rest of the crate;
//! no production code is replaced. No heap, IO, trait objects or concurrency are
//! modeled here; this pilot makes no claim for those Rust features.
#[path = "../../crates/norn-checker/src/exact_rational.rs"]
mod exact_rational;

#[kani::proof]
#[kani::unwind(3)]
fn integer_negation_correspondence() {
    let n: i64 = kani::any();
    let input = exact_rational::Rational {
        numerator: n,
        denominator: 1,
    };
    let actual = input.checked_neg();
    // i128 mathematical negation cannot overflow on the i64 domain.
    let mathematical = -i128::from(n);
    if mathematical > i128::from(i64::MAX) {
        assert!(actual.is_none());
    } else {
        let output = actual.expect("representable mathematical negation must succeed");
        assert_eq!(i128::from(output.numerator), mathematical);
        assert_eq!(output.denominator, 1);
    }
}
