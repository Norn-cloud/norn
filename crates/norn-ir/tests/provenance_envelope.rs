#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! The provenance envelope on instances (T8-ID-1, #2603, provenance part):
//! wire round-trip, closed shapes, omitted-vs-`null`, stable ids, and the
//! published schema.

use std::collections::BTreeSet;

use norn_ir::{
    FactRef, InstanceProvenance, MappingVersion, ProvenancedInstance, SourceRecordRef,
    UtcTimestamp, instance_provenance_schema,
};
use serde_json::{Value, json};

/// Two `ERPNext` customer rows resolved to one customer: two provenance
/// records on one fact.
fn merged_customer() -> Value {
    json!({
        "fact": { "objectType": "crm.Customer", "key": "C-77", "version": 1 },
        "provenance": [
            {
                "source": { "system": "ERPNext", "record": "Customer/CUST-77" },
                "mappingVersion": "erpnext.customer@2",
                "transform": "customer.header",
                "observedAt": "2026-09-30T08:00:00Z",
                "recordedAt": "2026-09-30T08:00:01.250Z"
            },
            {
                "source": { "system": "ERPNext", "record": "Customer/CUST-77-DUP" },
                "mappingVersion": "erpnext.customer@2",
                "recordedAt": "2026-09-30T08:00:02Z",
                "upstream": [
                    { "objectType": "crm.Customer", "key": "C-77", "version": 1 }
                ]
            }
        ]
    })
}

#[test]
fn provenanced_instance_round_trips_with_two_source_records() {
    let value = merged_customer();
    let instance: ProvenancedInstance =
        serde_json::from_value(value.clone()).expect("instance decodes");
    assert_eq!(instance.provenance.len(), 2);
    assert_eq!(instance.fact, FactRef::new("crm.Customer", "C-77", 1));
    let first = &instance.provenance[0];
    assert_eq!(
        first.source,
        SourceRecordRef::new("ERPNext", "Customer/CUST-77")
    );
    assert_eq!(
        first.mapping_version,
        MappingVersion("erpnext.customer@2".to_owned())
    );
    assert_eq!(first.transform.as_deref(), Some("customer.header"));
    assert_eq!(
        first.observed_at,
        Some(UtcTimestamp("2026-09-30T08:00:00Z".to_owned()))
    );
    assert!(first.upstream.is_empty());
    assert_eq!(instance.provenance[1].upstream.len(), 1);
    assert_eq!(
        serde_json::to_value(&instance).expect("instance encodes"),
        value,
        "canonical form omits absent members and empty upstream lists"
    );
}

#[test]
fn envelope_is_closed_and_rejects_explicit_null() {
    let base = json!({
        "source": { "system": "ERPNext", "record": "Sales Order/SO-1001" },
        "mappingVersion": "erpnext.sales_order@3",
        "recordedAt": "2026-09-30T08:00:00Z"
    });
    let decoded = serde_json::from_value::<InstanceProvenance>(base.clone())
        .expect("minimal envelope decodes with the pre-markings wire shape");
    assert!(decoded.source_fields.is_empty());
    assert!(decoded.markings.is_empty());
    assert_eq!(
        serde_json::to_value(decoded).expect("legacy provenance re-encodes"),
        base,
        "empty lineage metadata stays omitted for older canonical records"
    );

    for (member, value) in [
        ("transform", Value::Null),
        ("observedAt", Value::Null),
        ("unknown", json!("x")),
    ] {
        let mut invalid = base.clone();
        invalid[member] = value;
        assert!(
            serde_json::from_value::<InstanceProvenance>(invalid).is_err(),
            "{member} must be rejected"
        );
    }
    for required in ["source", "mappingVersion", "recordedAt"] {
        let mut invalid = base.clone();
        invalid
            .as_object_mut()
            .expect("object")
            .remove(required)
            .expect("member present");
        assert!(
            serde_json::from_value::<InstanceProvenance>(invalid).is_err(),
            "{required} is required"
        );
    }
    let mut source_extra = base;
    source_extra["source"]["table"] = json!("tabSales Order");
    assert!(serde_json::from_value::<InstanceProvenance>(source_extra).is_err());
}

#[test]
fn stable_ids_are_deterministic() {
    assert_eq!(
        FactRef::new("commerce.SalesOrder", "1001", 2).stable_id(),
        "fact:commerce.SalesOrder/1001@v2"
    );
    assert_eq!(
        FactRef::new("files.Path", "a/b@v1", 3).stable_id(),
        "fact:files.Path/a/b@v1@v3",
        "the version follows the last @v, so keys may contain / and @v"
    );
    assert_eq!(
        SourceRecordRef::new("ERPNext", "Sales Order/SO-1001").stable_id(),
        "source:ERPNext/Sales Order/SO-1001"
    );
}

#[test]
fn schema_publishes_required_members_and_timestamp_pattern() {
    let schema = serde_json::to_value(instance_provenance_schema()).expect("schema encodes");
    let required: BTreeSet<&str> = schema["required"]
        .as_array()
        .expect("required list")
        .iter()
        .map(|member| member.as_str().expect("member name"))
        .collect();
    assert_eq!(
        required,
        BTreeSet::from(["source", "mappingVersion", "recordedAt"])
    );
    assert_eq!(schema["additionalProperties"], json!(false));
    let text = schema.to_string();
    assert!(
        text.contains(
            norn_ir::UTC_TIMESTAMP_PATTERN
                .replace('\\', "\\\\")
                .as_str()
        ),
        "the timestamp grammar is published"
    );
}
