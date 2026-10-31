//! Integer-preserving storage uses the existing policy schema and no source IO.
#![allow(clippy::expect_used, reason = "bounded integer storage regressions")]

use norn_ir::{PayloadField, PolicyValue};

#[test]
fn policy_storage_preserves_the_full_long_range() {
    for value in [
        9_007_199_254_740_993_i64,
        1_152_921_504_606_846_976,
        i64::MIN,
        i64::MAX,
    ] {
        let source = serde_json::to_string(&value).expect("typed integer source");
        let stored: PolicyValue = serde_json::from_str(&source).expect("exact storage");
        assert_eq!(stored.decode::<i64>().expect("typed Long"), value);
        assert_eq!(
            serde_json::to_string(&stored).expect("exact serialization"),
            source
        );
        let field: PayloadField<i64, PolicyValue> =
            serde_json::from_str(&source).expect("lenient field");
        assert_eq!(
            field
                .as_value()
                .expect("present")
                .decode::<i64>()
                .expect("typed field"),
            value
        );
    }
    let out_of_range = serde_json::to_string(&u64::MAX).expect("typed unsigned source");
    let stored: PolicyValue =
        serde_json::from_str(&out_of_range).expect("lenient unsigned storage");
    assert!(stored.decode::<i64>().is_err());
    let fraction = serde_json::to_string(&0.5_f64).expect("typed fractional source");
    let stored: PolicyValue = serde_json::from_str(&fraction).expect("lenient fractional storage");
    assert!(stored.decode::<i64>().is_err());
}
