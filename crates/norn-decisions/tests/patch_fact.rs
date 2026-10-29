#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! #3080: a decide graph's `patch` record lowers to one canonical `set` per
//! property, each at its own type, in property-id order; other records decode
//! as one canonical fact change; malformed patches are refused.

use norn_decisions::lower_fact_record;
use norn_ir::{FactChange, FactChangeOp, RuntimeValue};

fn record(text: &str) -> RuntimeValue {
    RuntimeValue::from_json_text(text).expect("record")
}

#[test]
fn a_patch_lowers_to_one_set_per_property_in_property_order() {
    let changes = lower_fact_record(&record(
        r#"{"op":"patch","objectType":"stock.Bin","key":"ITEM-1@WH","patch":{"valuation_rate":12.5,"actual_qty":3,"last_voucher":"DN-1"}}"#,
    ))
    .expect("lowers");
    let properties: Vec<(&str, &RuntimeValue)> = changes
        .iter()
        .map(|change| match change {
            FactChange::Set {
                object_type,
                key,
                property,
                value,
            } => {
                assert_eq!(object_type.as_str(), "stock.Bin");
                assert_eq!(key, "ITEM-1@WH");
                (property.as_str(), value)
            }
            other @ (FactChange::Create { .. }
            | FactChange::Delete { .. }
            | FactChange::Transition { .. }) => panic!("not a set: {other:?}"),
        })
        .collect();
    assert_eq!(
        properties
            .iter()
            .map(|(property, _)| *property)
            .collect::<Vec<_>>(),
        ["actual_qty", "last_voucher", "valuation_rate"]
    );
    assert_eq!(properties[1].1.as_str(), Some("DN-1"));
    assert_eq!(
        properties[2].1,
        &record("12.5"),
        "a decimal is carried exactly"
    );
}

#[test]
fn other_records_decode_as_one_canonical_change() {
    let changes = lower_fact_record(&record(
        r#"{"op":"transition","objectType":"stock.DeliveryNote","key":"DN-1","lifecycle":"status","transition":"post"}"#,
    ))
    .expect("decodes");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].op(), FactChangeOp::Transition);
}

#[test]
fn malformed_patches_are_refused() {
    for (case, text) in [
        (
            "an empty patch",
            r#"{"op":"patch","objectType":"stock.Bin","key":"B","patch":{}}"#,
        ),
        (
            "a patch that is not a record",
            r#"{"op":"patch","objectType":"stock.Bin","key":"B","patch":3}"#,
        ),
        (
            "a member of another op",
            r#"{"op":"patch","objectType":"stock.Bin","key":"B","patch":{"a":1},"value":1}"#,
        ),
        (
            "no key",
            r#"{"op":"patch","objectType":"stock.Bin","patch":{"a":1}}"#,
        ),
    ] {
        assert!(lower_fact_record(&record(text)).is_err(), "{case}");
    }
    // `patch` is never a canonical fact change on its own.
    assert!(
        norn_ir::runtime::decode::<FactChange>(&record(
            r#"{"op":"patch","objectType":"stock.Bin","key":"B"}"#
        ))
        .is_err()
    );
}
