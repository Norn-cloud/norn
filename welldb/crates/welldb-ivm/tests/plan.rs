//! Typed synthetic plans; no IR, external records or authority assertions.
#![allow(clippy::expect_used, reason = "plan profile regression inputs")]
use welldb_ivm::{Aggregate, Field, Filter, Placement, Plan, Property};
fn plan() -> Plan {
    Plan { measure_type: "synthetic.Invoice".into(), link: None,
        fields: vec![Field { property: "at".into(), placement: Placement::Measure, kind: Property::Timestamp }],
        time: 0, group: None, aggregate: Aggregate::Count, filters: vec![], window: (0,100), min_group_size: 1, definition_version: 1 }
}
fn grouped() -> Plan {
    let mut p = plan();
    p.fields.push(Field { property: "customer".into(), placement: Placement::Measure, kind: Property::Text }); p.group = Some(1); p
}
#[test]
fn canonical_capture_order_and_closed_operands_are_required() {
    plan().validate().expect("count"); grouped().validate().expect("grouped count");
    let mut p = plan(); p.window = (1,1); assert!(p.validate().is_err());
    let mut p = grouped(); p.fields.swap(0,1); p.time = 1; p.group = Some(0); assert!(p.validate().is_err());
    let mut p = grouped(); p.fields.get_mut(1).expect("group").property = "at".into(); assert!(p.validate().is_err());
    let mut p = grouped(); p.filters = vec![Filter { field: 1, equal: true, value: "synthetic".into() }; 17]; assert!(p.validate().is_err());
    let mut p = plan(); p.fields.push(Field { property: "unused".into(), placement: Placement::Measure, kind: Property::Text }); assert!(p.validate().is_err());
    let mut p = plan(); p.fields.get_mut(0).expect("time").placement = Placement::Target; assert!(p.validate().is_err());
}
#[test]
fn fixed_scale_sum_refuses_an_amount_outside_its_declared_profile() {
    let mut p = plan(); p.fields.push(Field { property: "amount".into(), placement: Placement::Measure, kind: Property::Decimal { scale: 2 } });
    p.aggregate = Aggregate::Sum { amount: 1, scale: 2, currency: None }; p.validate().expect("sum");
    p.aggregate = Aggregate::Sum { amount: 1, scale: 3, currency: None }; assert!(p.validate().is_err());
    p.aggregate = Aggregate::Sum { amount: 0, scale: 2, currency: None }; assert!(p.validate().is_err());
}
#[cfg(feature = "serde")]
#[test]
fn typed_plan_roundtrip_preserves_the_profile() {
    let original = grouped();
    let bytes = serde_json::to_vec(&original).expect("encode");
    let decoded: Plan = serde_json::from_slice(&bytes).expect("decode");
    assert_eq!(decoded, original); decoded.validate().expect("profile retained");
}
