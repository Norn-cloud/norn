//! Independent refusal-order and exact-output oracles over synthetic buckets.
#![allow(clippy::expect_used, reason = "bounded exact output oracles")]
use std::collections::BTreeMap;
use welldb_ivm::{Aggregate, BucketAgg, CaptureKey, Decimal, ExactSum, Field, MetricError, ObjectKey, Placement, Plan, Property, finalize};
fn plan() -> Plan { Plan { measure_type: "synthetic.Invoice".into(), link: None, fields: vec![Field { property: "at".into(), placement: Placement::Measure, kind: Property::Timestamp }], time: 0, group: None, aggregate: Aggregate::Count, filters: vec![], window: (0, 1), min_group_size: 2, definition_version: 1 } }
#[test]
fn empty_ungrouped_populations_are_checked_while_absent_grouped_buckets_are_omitted() {
    let mut p = plan();
    assert_eq!(finalize(&p, &BTreeMap::new(), &BTreeMap::new()), Err(MetricError::Disclosure));
    p.min_group_size = 0;
    let output = finalize(&p, &BTreeMap::new(), &BTreeMap::new()).expect("empty count");
    assert_eq!(output.len(), 1); assert_eq!(output.first().expect("bucket").value, Decimal::new(0, 0).expect("zero"));
    p.group = Some(1); p.min_group_size = 2;
    assert!(finalize(&p, &BTreeMap::new(), &BTreeMap::new()).expect("empty grouped").is_empty());
}
#[test]
fn capture_phase_precedes_row_validity_and_group_disclosure_precedes_decimal_conversion() {
    let mut p = plan(); p.group = Some(1);
    let huge = ExactSum::from_i128(1_i128 << 100);
    let buckets = BTreeMap::from([(Some("a".into()), BucketAgg { sum: huge, population: 1 }), (Some("b".into()), BucketAgg { sum: huge, population: 2 })]);
    assert_eq!(finalize(&p, &BTreeMap::new(), &buckets), Err(MetricError::Disclosure));
    let key = |phase, id: &str| CaptureKey { phase, measure: ObjectKey { object_type: p.measure_type.clone(), id: id.into() }, field: 0 };
    let refusals = BTreeMap::from([(key(1,"a"), MetricError::InvalidField), (key(0,"z"), MetricError::MarkingDenied)]);
    assert_eq!(finalize(&p, &refusals, &buckets), Err(MetricError::MarkingDenied));
    let overflow = BTreeMap::from([(Some("b".into()), BucketAgg { sum: huge, population: 2 })]);
    assert_eq!(finalize(&p, &BTreeMap::new(), &overflow), Err(MetricError::AggregateOverflow));
    let population = BTreeMap::from([(None, BucketAgg { sum: ExactSum::ZERO, population: u64::MAX })]);
    assert_eq!(finalize(&p, &BTreeMap::new(), &population), Err(MetricError::PopulationOverflow));
}
#[test]
fn fixed_scale_output_is_normalized_without_rounding_and_retains_currency() {
    let mut p = plan(); p.aggregate = Aggregate::Sum { amount: 1, scale: 2, currency: Some((2,"SYN".into())) };
    let buckets = BTreeMap::from([(None, BucketAgg { sum: ExactSum::from_i128(12_300), population: 2 })]);
    let result = finalize(&p, &BTreeMap::new(), &buckets).expect("exact sum");
    let bucket = result.first().expect("bucket");
    assert_eq!(bucket.value, Decimal::new(123,0).expect("normalized")); assert_eq!(bucket.currency.as_deref(), Some("SYN")); assert_eq!(bucket.population, 2);
}
