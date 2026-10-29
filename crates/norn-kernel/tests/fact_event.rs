//! Exact, closed wire decoding for typed ledger events.

use std::collections::BTreeMap;
use std::error::Error;

use norn_ir::{RuntimeValue, runtime};
use norn_kernel::FactEvent;

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
