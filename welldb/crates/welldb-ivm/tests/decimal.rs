//! Synthetic decimal boundary cases, authored as typed Rust values.
#![allow(clippy::expect_used, reason = "typed decimal regression inputs")]
use serde::Serialize;
use welldb_ivm::{Decimal, MetricError};

#[test]
fn normalization_and_exact_domain_limits_are_enforced() {
    assert_eq!(Decimal::new(120, 2), Decimal::new(12, 1));
    assert_eq!(Decimal::new(0, 28), Decimal::new(0, 0));
    assert_eq!(Decimal::new(1, 29), Err(MetricError::AggregateOverflow));
    assert_eq!(Decimal::new(1_i128 << 96, 0), Err(MetricError::AggregateOverflow));
    let value = Decimal::new(1, 28).expect("finest scale");
    assert_eq!(value.mantissa(), 1);
    assert_eq!(value.scale(), 28);
}

#[derive(Serialize)]
struct Wire { mantissa: i128, scale: u8 }

#[test]
fn serde_cannot_bypass_normalization_or_domain_validation() {
    for wire in [Wire { mantissa: 1, scale: 29 }, Wire { mantissa: 1_i128 << 96, scale: 0 }] {
        let encoded = serde_json::to_string(&wire).expect("encode typed input");
        assert!(serde_json::from_str::<Decimal>(&encoded).is_err());
    }
    let encoded = serde_json::to_string(&Wire { mantissa: 120, scale: 2 }).expect("encode");
    assert_eq!(serde_json::from_str::<Decimal>(&encoded).expect("decode"), Decimal::new(12, 1).expect("normalize"));
}
