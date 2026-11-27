//! Exact, closed wire decoding for typed ledger events.

use std::collections::BTreeMap;
use std::error::Error;

use norn_ir::provenance::{InstanceProvenance, MappingVersion, SourceRecordRef, UtcTimestamp};
use norn_ir::{
    DataQualityFinding, DataQualityFindingStatus, DataQualityOutcome, DataQualityRuleRef,
    DataQualitySeverity, DataQualitySubject, ObjectTypeName, RuntimeValue, runtime,
};
use norn_kernel::{FactEvent, ObservationEvent};

type TestResult = Result<(), Box<dyn Error>>;

const KEY: &str = r#"{"objectType":"test.OrderLine","key":"line-1"}"#;
const EXACT: &str = r#"{
    "integer":9007199254740993,
    "unsigned":18446744073709551615,
    "nested":[null,-9007199254740993,{"decimal":0.1234567890123456789012345678}]
}"#;

fn event_jsons() -> [String; 11] {
    [
        format!(
            r#"{{"op":"command_enqueued","commandId":"command-1","generation":18446744073709551615,
                "idempotencyKey":"request-1","actionCall":"call-1","payload":{EXACT}}}"#
        ),
        format!(
            r#"{{"op":"naming_series_allocated","key":{KEY},"counter":{KEY},
                "pattern":"ROW-{{company}}-{{#####}}","timezone":"UTC","epochMillis":18446744073709551615,
                "tzdbVersion":"2026a","scope":{{"company":"ACME"}},"period":"2026",
                "value":18446744073709551615,"resetPolicy":"year","gapPolicy":"gapless_within_commit"}}"#
        ),
        format!(
            r#"{{"op":"naming_series_counter_seeded","counter":{KEY},"objectType":"test.OrderLine",
                "keyName":"primary","scope":{EXACT},"period":"2026","current":18446744073709551615}}"#
        ),
        format!(
            r#"{{"op":"naming_series_counter_seeded","counter":{KEY},"objectType":"test.OrderLine",
                "keyName":"primary","scope":null,"period":"","current":0}}"#
        ),
        format!(r#"{{"op":"created","key":{KEY},"values":{EXACT}}}"#),
        format!(r#"{{"op":"set","key":{KEY},"property":"data","value":{EXACT}}}"#),
        format!(r#"{{"op":"set","key":{KEY},"property":"data","value":null}}"#),
        format!(
            r#"{{"op":"modified","key":{KEY},"beforeVersion":9007199254740993,
                "afterVersion":9007199254740994,"before":{EXACT},"after":{{"data":{EXACT}}},
                "changedProperties":["data","integer","nested","unsigned"]}}"#
        ),
        format!(r#"{{"op":"deleted","key":{KEY}}}"#),
        format!(
            r#"{{"op":"transitioned","key":{KEY},"lifecycle":"status","transition":"submit",
                "from":"draft","to":"submitted"}}"#
        ),
        format!(
            r#"{{"op":"invariant_refused","key":{KEY},"owner":{{"kind":"object","name":"test.OrderLine"}},
                "id":"positive","message":"Quantity must be positive",
                "outcome":{{"kind":"non_boolean","actual":"null"}},"attemptId":"attempt-1"}}"#
        ),
    ]
}

#[test]
fn every_variant_round_trips_exact_values_through_the_ledger_codec() -> TestResult {
    for text in event_jsons() {
        let expected = RuntimeValue::from_json_text(&text)?;
        let event: FactEvent = serde_json::from_str(&text)?;
        assert_eq!(runtime::encode(&event)?, expected, "wire changed: {text}");
        // SqliteCell::commits uses this same decode boundary. Serialization
        // sorts fields, so decoding also exercises fields before the op tag.
        let replayed: FactEvent = runtime::decode(&expected)?;
        assert_eq!(replayed, event);
    }
    Ok(())
}

#[test]
fn each_variant_requires_every_member_and_rejects_nonnullable_nulls() -> TestResult {
    for text in event_jsons() {
        let record = RuntimeValue::from_json_text(&text)?;
        let fields = record.as_object().ok_or("event must be an object")?;
        for name in fields.keys() {
            let mut missing = fields.clone();
            missing.remove(name);
            assert!(
                runtime::decode::<FactEvent>(&RuntimeValue::Object(missing)).is_err(),
                "accepted missing {name}: {text}"
            );
            // Set.value and CounterSeeded.scope carry the whole runtime domain.
            let untyped = (name == "value"
                && fields.get("op").and_then(RuntimeValue::as_str) == Some("set"))
                || (name == "scope"
                    && fields.get("op").and_then(RuntimeValue::as_str)
                        == Some("naming_series_counter_seeded"));
            if !untyped {
                for invalid in [RuntimeValue::Null, RuntimeValue::Bool(true)] {
                    let mut malformed = fields.clone();
                    malformed.insert(name.clone(), invalid);
                    assert!(
                        runtime::decode::<FactEvent>(&RuntimeValue::Object(malformed)).is_err(),
                        "accepted malformed {name}: {text}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
fn variants_reject_unknown_and_cross_variant_fields_even_when_null() -> TestResult {
    let mut known_fields = BTreeMap::new();
    for text in event_jsons() {
        let record = RuntimeValue::from_json_text(&text)?;
        known_fields.extend(record.as_object().ok_or("event must be an object")?.clone());
    }
    known_fields.insert("unknownField".to_owned(), RuntimeValue::Bool(true));
    for text in event_jsons() {
        let record = RuntimeValue::from_json_text(&text)?;
        let fields = record.as_object().ok_or("event must be an object")?;
        for (name, value) in &known_fields {
            if fields.contains_key(name) {
                continue;
            }
            for extra in [value.clone(), RuntimeValue::Null] {
                let mut extended = fields.clone();
                extended.insert(name.clone(), extra);
                assert!(
                    runtime::decode::<FactEvent>(&RuntimeValue::Object(extended)).is_err(),
                    "accepted extra {name}: {text}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn duplicate_members_and_unknown_operations_are_rejected() -> TestResult {
    for text in event_jsons() {
        let record = RuntimeValue::from_json_text(&text)?;
        let fields = record.as_object().ok_or("event must be an object")?;
        for (name, value) in fields {
            // Feed the text directly: a map would erase the duplicate before
            // it reached the wire decoder, including duplicate null values.
            let duplicate = format!(
                "{{{}:{},{}",
                serde_json::to_string(name)?,
                serde_json::to_string(value)?,
                text.strip_prefix('{').ok_or("event must start with {")?
            );
            assert!(
                serde_json::from_str::<FactEvent>(&duplicate).is_err(),
                "accepted duplicate {name}: {duplicate}"
            );
        }
        let mut unknown = fields.clone();
        unknown.insert("op".to_owned(), RuntimeValue::from("patched"));
        assert!(runtime::decode::<FactEvent>(&RuntimeValue::Object(unknown)).is_err());
    }
    Ok(())
}

#[test]
fn wire_requires_an_object_and_a_string_operation() {
    for text in [
        "null",
        "true",
        "1",
        r#""deleted""#,
        r#"["command_enqueued","cmd",1,"idem","call",{}]"#,
        r#"{"op":{"deleted":null},"key":{"objectType":"test.OrderLine","key":"line-1"}}"#,
    ] {
        assert!(
            serde_json::from_str::<FactEvent>(text).is_err(),
            "accepted malformed event: {text}"
        );
    }
}

#[test]
fn allocation_value_and_scope_keep_their_narrow_types() -> TestResult {
    for text in event_jsons() {
        let record = RuntimeValue::from_json_text(&text)?;
        let fields = record.as_object().ok_or("event must be an object")?;
        if fields.get("op").and_then(RuntimeValue::as_str) != Some("naming_series_allocated") {
            continue;
        }
        for (name, invalid) in [
            ("value", "-1"),
            ("value", "1.5"),
            ("value", "18446744073709551616"),
            ("value", r#""1""#),
            ("scope", r#"{"company":1}"#),
            ("scope", "[]"),
        ] {
            let mut malformed = fields.clone();
            malformed.insert(name.to_owned(), RuntimeValue::from_json_text(invalid)?);
            assert!(runtime::decode::<FactEvent>(&RuntimeValue::Object(malformed)).is_err());
        }
    }
    Ok(())
}

fn sample_provenance() -> InstanceProvenance {
    InstanceProvenance {
        source: SourceRecordRef::new("ERPNext", "OrderLine/line-1"),
        mapping_version: MappingVersion("erpnext.norn_core@0.2.0".to_owned()),
        source_fields: Vec::new(),
        markings: Vec::new(),
        transform: None,
        observed_at: None,
        recorded_at: UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        upstream: Vec::new(),
    }
}

fn observation_event_jsons() -> Result<Vec<String>, Box<dyn Error>> {
    let prov = serde_json::to_string(&sample_provenance())?;
    let finding = DataQualityFinding {
        schema_version: 2,
        id: "dq-1".to_owned(),
        dedupe_key: "dedupe-1".to_owned(),
        evidence_digest: None,
        rule_ref: DataQualityRuleRef::Builtin {
            id: "test.rule".to_owned(),
            version: "1".to_owned(),
        },
        subjects: vec![DataQualitySubject::new(
            ObjectTypeName("test.OrderLine".to_owned()),
            "line-1",
        )],
        severity: DataQualitySeverity::Warning,
        message: "Quantity out of range".to_owned(),
        outcome: DataQualityOutcome::NotSatisfied,
        provenance: Vec::new(),
        first_seen: UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        last_seen: UtcTimestamp("2026-10-01T00:00:00Z".to_owned()),
        status: DataQualityFindingStatus::Open,
        links: BTreeMap::new(),
        values: BTreeMap::from([(
            "amount".to_owned(),
            RuntimeValue::from_json_text("0.1234567890123456789012345678")?,
        )]),
    };
    let finding_json = serde_json::to_string(&finding)?;
    Ok(vec![
        format!(
            r#"{{"op":"observed_fact","event":{{"op":"created","key":{KEY},"values":{EXACT}}},"provenance":{prov}}}"#
        ),
        format!(
            r#"{{"op":"observed_fact","event":{{"op":"set","key":{KEY},"property":"amount","value":0.1234567890123456789012345678}},"provenance":{prov}}}"#
        ),
        format!(
            r#"{{"op":"observed_transition","key":{KEY},"property":"status","transition":"submit","from":"draft","to":"submitted","synthesized":"snapshot_catch_up","provenance":{prov}}}"#
        ),
        format!(r#"{{"op":"data_quality_finding","fact":{finding_json}}}"#),
        format!(r#"{{"op":"observed_deleted","key":{KEY},"tombstoned":true,"provenance":{prov}}}"#),
        format!(r#"{{"op":"observed_recreated","key":{KEY},"provenance":{prov}}}"#),
    ])
}

#[test]
fn observation_event_variants_round_trip_exact_numbers_and_closed_wire() -> TestResult {
    let samples = observation_event_jsons()?;
    let mut known_fields = BTreeMap::new();
    for text in &samples {
        let expected = RuntimeValue::from_json_text(text)?;
        let event: ObservationEvent = serde_json::from_str(text)?;
        assert_eq!(runtime::encode(&event)?, expected, "wire changed: {text}");
        let replayed: ObservationEvent = runtime::decode(&expected)?;
        assert_eq!(replayed, event);
        known_fields.extend(
            expected
                .as_object()
                .ok_or("observation event must be an object")?
                .clone(),
        );
    }
    known_fields.insert("unknownField".to_owned(), RuntimeValue::Bool(true));

    for text in &samples {
        let record = RuntimeValue::from_json_text(text)?;
        let fields = record
            .as_object()
            .ok_or("observation event must be an object")?;
        for name in fields.keys() {
            let mut missing = fields.clone();
            missing.remove(name);
            assert!(
                runtime::decode::<ObservationEvent>(&RuntimeValue::Object(missing)).is_err(),
                "accepted missing {name}: {text}"
            );
            let mut nulled = fields.clone();
            nulled.insert(name.clone(), RuntimeValue::Null);
            assert!(
                runtime::decode::<ObservationEvent>(&RuntimeValue::Object(nulled)).is_err(),
                "accepted null {name}: {text}"
            );
        }
        for (name, value) in &known_fields {
            if fields.contains_key(name) {
                continue;
            }
            for extra in [value.clone(), RuntimeValue::Null] {
                let mut extended = fields.clone();
                extended.insert(name.clone(), extra);
                assert!(
                    runtime::decode::<ObservationEvent>(&RuntimeValue::Object(extended)).is_err(),
                    "accepted extra {name}: {text}"
                );
            }
        }
    }

    for malformed in [
        "null",
        r#"["observed_recreated"]"#,
        r#"{"op":"unknown_variant"}"#,
    ] {
        assert!(serde_json::from_str::<ObservationEvent>(malformed).is_err());
    }
    Ok(())
}

#[test]
fn tagged_events_serialize_decimals_honestly_with_serde_json_to_value() -> TestResult {
    let prov = serde_json::to_string(&sample_provenance())?;
    let exact_binary_fact: FactEvent = serde_json::from_str(&format!(
        r#"{{"op":"set","key":{KEY},"property":"amount","value":42.25}}"#
    ))?;
    let fact_val = serde_json::to_value(&exact_binary_fact)?;
    assert_eq!(
        fact_val.get("value").and_then(serde_json::Value::as_f64),
        Some(42.25)
    );

    let exact_binary_obs: ObservationEvent = serde_json::from_str(&format!(
        r#"{{"op":"observed_fact","event":{{"op":"set","key":{KEY},"property":"amount","value":42.25}},"provenance":{prov}}}"#
    ))?;
    let obs_val = serde_json::to_value(&exact_binary_obs)?;
    assert_eq!(
        obs_val
            .get("event")
            .and_then(|event| event.get("value"))
            .and_then(serde_json::Value::as_f64),
        Some(42.25)
    );

    // High-precision decimals that f64 cannot represent exactly must error on
    // `serde_json::to_value` rather than silently rounding or leaking RawValue.
    let high_precision_obs: ObservationEvent = serde_json::from_str(&format!(
        r#"{{"op":"observed_fact","event":{{"op":"set","key":{KEY},"property":"amount","value":0.1234567890123456789012345678}},"provenance":{prov}}}"#
    ))?;
    assert!(serde_json::to_value(&high_precision_obs).is_err());
    Ok(())
}
