//! Synthetic exact-sum/retraction boundary cases; no external records.
#![allow(clippy::expect_used, reason = "bounded arithmetic regression inputs")]
use welldb_ivm::{BucketAgg, Decimal, ExactSum, MetricError};

#[test]
fn normalized_decimals_retract_exactly_and_refuse_precision_loss() {
    let decimal = Decimal::new(120, 2).expect("decimal");
    assert_eq!(decimal, Decimal::new(12, 1).expect("normalized"));
    assert_eq!(ExactSum::scaled(decimal, 0), Err(MetricError::InvalidField));
    let contribution = BucketAgg { sum: ExactSum::scaled(decimal, 2).expect("scale"), population: 1 };
    let mut total = BucketAgg::default();
    total.add(&contribution).expect("add");
    total.retract(&contribution).expect("retract");
    assert_eq!(total, BucketAgg::default());
    assert_eq!(total.retract(&contribution), Err(MetricError::PopulationOverflow));
    assert_eq!(total, BucketAgg::default());
}

#[test]
fn wide_sum_is_order_independent_and_refuses_out_of_domain_parts() {
    let a = ExactSum::from_i128(i128::MAX);
    let b = ExactSum::from_i128(i128::MIN);
    assert_eq!(a.checked_add(b), b.checked_add(a));
    assert_eq!(a.checked_add(b), Some(ExactSum::from_i128(-1)));
    assert!(ExactSum::from_parts(false, [0, 0, 0, 1 << 63]).is_err());
    assert_eq!(ExactSum::from_parts(true, [0; 4]).expect("negative zero"), ExactSum::ZERO);
}
