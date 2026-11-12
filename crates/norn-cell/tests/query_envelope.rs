#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "exact wire conformance assertions"
)]
use norn_cell::compute::{Frontier, Member, QueryRow};
use norn_ir::{Decimal, RuntimeValue};
mod query_support;
use std::collections::BTreeMap;

#[test]
fn query_envelope_preserves_exact_contributions_and_unavailable_ratios() {
    let amount = Decimal::parse("9007199254740993.125").expect("exact");
    let row = QueryRow {
        bucket: RuntimeValue::from("2026-10-19"),
        totals: vec![amount],
        value: None,
        members: BTreeMap::from([(
            "conversion-1".into(),
            Member {
                version: 7,
                amounts: Some(vec![Some(amount)]),
                restricted: false,
            },
        )]),
        suppressed: false,
        null_counts: Vec::new(),
    };
    let wire = serde_json::to_string(&row).expect("exact transport");
    assert!(wire.contains("9007199254740993.125"));
    assert!(wire.contains("\"value\":null"));
    // No null skipped: no `nullCounts` on the wire, so complete-data answers are unchanged.
    assert!(!wire.contains("nullCounts"));
    assert_eq!(
        serde_json::from_str::<QueryRow>(&wire).expect("decode"),
        row
    );
}

#[test]
fn source_frontier_keeps_connector_partitions_and_opaque_cursors_separate() {
    let frontier: Frontier = BTreeMap::from([
        (
            "erp".into(),
            BTreeMap::from([
                ("north".into(), "opaque:z".into()),
                ("south".into(), "opaque:a".into()),
            ]),
        ),
        (
            "shop".into(),
            BTreeMap::from([("catalog".into(), "different-format/7".into())]),
        ),
    ]);
    let wire = serde_json::to_string(&frontier).expect("encode");
    assert_eq!(
        serde_json::from_str::<Frontier>(&wire).expect("decode"),
        frontier
    );
}

#[test]
fn declared_scrap_yield_fixture_admits_with_the_pinned_result_context() {
    let (_, plan, context) = query_support::fixture();
    assert_eq!(plan.grain().as_str(), "demo.Conversion");
    let wire = serde_json::to_string(&context).expect("context");
    assert_eq!(
        serde_json::from_str::<norn_cell::compute::QueryContext>(&wire).expect("decode"),
        context
    );
}
