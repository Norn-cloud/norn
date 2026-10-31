//! Synthetic row capture checks disclosure ordering before selection and exact constraints.
#![allow(clippy::expect_used, reason = "bounded capture regression inputs")]
use std::collections::{BTreeMap, BTreeSet};
use welldb_ivm::{Access, Aggregate, ClassId, Decimal, Field, JoinedTarget, LinkSpec, MetricError, ObjectKey, Placement, Plan, Property, Record, RowRef, Value, VisibilityOutcome, capture};
fn class() -> ClassId { ClassId { actor: "synthetic-analyst".into(), epoch: 1 } }
fn key(kind: &str) -> ObjectKey { ObjectKey { object_type: kind.into(), id: "synthetic-1".into() } }
fn field(property: &str, kind: Property) -> Field { Field { property: property.into(), placement: Placement::Measure, kind } }
fn plan() -> Plan { Plan { measure_type: "synthetic.Invoice".into(), link: None, fields: vec![field("at", Property::Timestamp)], time: 0, group: None, aggregate: Aggregate::Count, filters: vec![], window: (0, 86_400_000_000_000), min_group_size: 1, definition_version: 1 } }
fn record() -> Record { Record { values: BTreeMap::from([("at".into(), Value::Text("1970-01-01T00:00:00Z".into()))]), access: BTreeMap::from([(class(), Access { visible: true, masked: BTreeSet::new() })]), seq: 7, hash: "synthetic-receipt-7".into() } }
fn row<'a>(key: &'a ObjectKey, record: &'a Record) -> RowRef<'a> { RowRef { key, record, tombstoned: false } }
#[test]
fn masking_and_validity_are_checked_even_for_rows_outside_the_window() {
    let p = plan(); p.validate().expect("admitted shape"); let id = key(&p.measure_type); let mut r = record();
    r.values.insert("at".into(), Value::Text("1970-01-02T00:00:00Z".into()));
    let result = capture(&p, &class(), row(&id,&r), JoinedTarget::NotRequired);
    assert!(result.contribution.is_none()); assert!(result.refusal.is_none());
    r.access.get_mut(&class()).expect("access").masked.insert("at".into());
    let refusal = capture(&p, &class(), row(&id,&r), JoinedTarget::NotRequired).refusal.expect("masked");
    assert_eq!(refusal.kind, MetricError::MarkingDenied); assert_eq!(refusal.key.phase, 0);
    r.access.get_mut(&class()).expect("access").masked.clear(); r.values.insert("at".into(), Value::Bool(false));
    let refusal = capture(&p, &class(), row(&id,&r), JoinedTarget::NotRequired).refusal.expect("invalid");
    assert_eq!(refusal.kind, MetricError::InvalidField); assert_eq!(refusal.key.phase, 1);
}
#[test]
fn hidden_target_short_circuits_later_masks_but_missing_links_refuse_at_capture_position() {
    let mut p = plan(); p.link = Some(LinkSpec { role: "customer".into(), target_type: "synthetic.Customer".into() });
    let mut group = field("name", Property::Text); group.placement = Placement::Target;
    p.fields.extend([group, field("amount", Property::Decimal { scale: 2 })]); p.group = Some(1); p.aggregate = Aggregate::Sum { amount: 2, scale: 2, currency: None }; p.validate().expect("admitted shape");
    let id = key(&p.measure_type); let target_id = key("synthetic.Customer"); let mut r = record(); let mut target = record();
    r.values.insert("amount".into(), Value::Decimal(Decimal::new(123,2).expect("amount"))); r.access.get_mut(&class()).expect("access").masked.insert("amount".into());
    target.values.insert("name".into(), Value::Text("Synthetic Customer".into())); target.access.clear();
    let result = capture(&p,&class(),row(&id,&r),JoinedTarget::Row(row(&target_id,&target)));
    assert_eq!(result.visibility, VisibilityOutcome::HiddenTarget); assert!(result.refusal.is_none()); assert!(result.row.is_none());
    let refusal = capture(&p,&class(),row(&id,&r),JoinedTarget::Missing).refusal.expect("link"); assert_eq!(refusal.kind, MetricError::SnapshotLink); assert_eq!(refusal.key.field, 1);
    target.access = record().access;
    let refusal = capture(&p,&class(),row(&id,&r),JoinedTarget::Row(row(&target_id,&target))).refusal.expect("later mask"); assert_eq!(refusal.kind,MetricError::MarkingDenied); assert_eq!(refusal.key.field,2);
    r.access.clear(); let result = capture(&p,&class(),row(&id,&r),JoinedTarget::Missing); assert_eq!(result.visibility,VisibilityOutcome::HiddenMeasure); assert!(result.refusal.is_none());
}
