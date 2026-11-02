//! SNAPSHOT-RECEIPT-1: a co-located hash cannot replace a receipt commitment.
/// The actual digest is computed from the complete canonical snapshot preimage.
pub(crate) fn receipt_digest_matches(expected: Option<&str>, actual: &str) -> bool {
    expected == Some(actual)
}

#[cfg(test)]
mod tests {
    use super::receipt_digest_matches;
    use norn_testkit::proptest::prelude::*;
    proptest! {
        #[test]
        fn only_the_exact_bound_digest_accepts(expected in ".{0,128}", actual in ".{0,128}") {
            prop_assert!(!receipt_digest_matches(None, &actual));
            prop_assert!(receipt_digest_matches(Some(&actual), &actual));
            prop_assert_eq!(receipt_digest_matches(Some(&expected), &actual), expected == actual);
        }
    }
}
