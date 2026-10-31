//! Synthetic exact-sum/retraction boundary cases; no external records.
#![allow(clippy::expect_used, reason = "bounded arithmetic regression inputs")]
use welldb_ivm::{BucketAgg, Decimal, ExactSum, MetricError};

#[test]
fn normalized_decimals_retract_exactly_and_refuse_precision_loss() {
    let decimal = Decimal::new(120, 2).expect("decimal");
    assert_eq!(decimal, Decimal::new(12, 1).expect("normalized"));
    assert_eq!(ExactSum::scaled(decimal, 0), Err(MetricError::InvalidField));
    let contribution = BucketAgg {
        sum: ExactSum::scaled(decimal, 2).expect("scale"),
        population: 1,
    };
    let mut total = BucketAgg::default();
    total.add(&contribution).expect("add");
    total.retract(&contribution).expect("retract");
    assert_eq!(total, BucketAgg::default());
    assert_eq!(
        total.retract(&contribution),
        Err(MetricError::PopulationOverflow)
    );
    assert_eq!(total, BucketAgg::default());
}

#[test]
fn wide_sum_is_order_independent_and_refuses_out_of_domain_parts() {
    let a = ExactSum::from_i128(i128::MAX);
    let b = ExactSum::from_i128(i128::MIN);
    assert_eq!(a.checked_add(b), b.checked_add(a));
    assert_eq!(a.checked_add(b), Some(ExactSum::from_i128(-1)));
    assert!(ExactSum::from_parts(false, [0, 0, 0, 1 << 63]).is_err());
    assert_eq!(
        ExactSum::from_parts(true, [0; 4]).expect("negative zero"),
        ExactSum::ZERO
    );
}

#[test]
fn wide_carry_borrow_and_population_overflow_preserve_the_domain() {
    let low = ExactSum::from_parts(false, [u64::MAX, 0, 0, 0]).expect("low limb");
    let one = ExactSum::from_i128(1);
    let carried = low.checked_add(one).expect("carry");
    assert_eq!(carried.to_parts(), (false, [0, 1, 0, 0]));
    assert_eq!(carried.checked_add(one.negated()), Some(low));
    let max = ExactSum::from_parts(false, [u64::MAX, u64::MAX, u64::MAX, u64::MAX >> 1])
        .expect("maximum");
    assert_eq!(max.checked_add(one), None);
    let mut total = BucketAgg {
        sum: ExactSum::ZERO,
        population: u64::MAX,
    };
    let before = total;
    assert_eq!(
        total.add(&BucketAgg {
            sum: one,
            population: 1
        }),
        Err(MetricError::PopulationOverflow)
    );
    assert_eq!(total, before);
}

#[cfg(feature = "serde")]
#[test]
fn wide_wire_rejects_out_of_domain_values_and_normalizes_negative_zero() {
    #[derive(serde::Serialize)]
    struct Wire {
        negative: bool,
        limbs: [u64; 4],
    }
    let invalid = serde_json::to_string(&Wire {
        negative: false,
        limbs: [0, 0, 0, 1 << 63],
    })
    .expect("typed wire");
    assert!(serde_json::from_str::<ExactSum>(&invalid).is_err());
    let zero = serde_json::to_string(&Wire {
        negative: true,
        limbs: [0; 4],
    })
    .expect("zero wire");
    assert_eq!(
        serde_json::from_str::<ExactSum>(&zero).expect("normalized"),
        ExactSum::ZERO
    );
}
