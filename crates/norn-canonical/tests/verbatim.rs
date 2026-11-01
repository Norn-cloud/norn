//! Exact commitments retain the pre-routing Serde bytes under every active
//! map/number feature profile. The legacy serialization oracle lives only here.
#![allow(clippy::expect_used, reason = "integration-test crate")]

use norn_canonical::{
    CanonicalJsonError, canonical_json_of, parse_json_verbatim, serialize_json_verbatim,
};
use serde::Serialize;
use serde_json::{Map, Value, json};

fn assert_legacy_bytes<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
    let legacy = serde_json::to_vec(value).expect("pre-change serialization");
    let owned = serialize_json_verbatim(value).expect("canonical-owned serialization");
    assert_eq!(owned, legacy);
    owned
}

#[test]
fn existing_law_fixtures_keep_every_serialized_byte() {
    for source in [
        include_str!("../../../fixtures/law-ir/entities.json"),
        include_str!("../../../fixtures/law-ir/laws.json"),
        include_str!("../../../fixtures/law-ir/statements.json"),
    ] {
        let value: Value = serde_json::from_str(source).expect("existing fixture JSON");
        let bytes = assert_legacy_bytes(&value);
        assert_eq!(
            parse_json_verbatim(&bytes).expect("serialized fixture"),
            parse_json_verbatim(source).expect("source fixture")
        );
        for artifact in value.as_array().expect("fixture array") {
            assert_legacy_bytes(artifact);
        }
    }
}

#[test]
fn typed_field_order_and_unsized_strings_are_verbatim() {
    // No Norn dependency: this stands for a typed envelope, whose declared
    // field order differs from the Norn1 key order even with BTreeMap enabled.
    #[derive(Serialize)]
    struct Envelope<'a> {
        z_expression: &'a str,
        a_identifier: &'a str,
    }
    let value = Envelope {
        z_expression: "len('e\u{301}') == 3",
        a_identifier: "ا\u{654}",
    };
    assert_eq!(
        assert_legacy_bytes(&value),
        "{\"z_expression\":\"len('e\u{301}') == 3\",\"a_identifier\":\"ا\u{654}\"}".as_bytes()
    );
    assert_ne!(
        serialize_json_verbatim(&value).expect("verbatim"),
        canonical_json_of(&value).expect("Norn1")
    );
    assert_eq!(assert_legacy_bytes("e\u{301}"), "\"e\u{301}\"".as_bytes());
}

#[test]
fn nfc_equivalent_literals_identifiers_and_keys_remain_distinct() {
    let cases: [fn(&str) -> Value; 3] = [
        |text| json!({"expression":format!("len('{text}') == 3")}),
        |text| json!({"identifier":text}),
        |text| json!({text:"value"}),
    ];
    for (composed, decomposed) in [("é", "e\u{301}"), ("أ", "ا\u{654}")] {
        for make in cases {
            let left = make(composed);
            let right = make(decomposed);
            assert_ne!(assert_legacy_bytes(&left), assert_legacy_bytes(&right));
            assert_eq!(
                canonical_json_of(&left).expect("Norn1"),
                canonical_json_of(&right).expect("Norn1")
            );
        }
    }
    // Exact authority profiles can retain both keys; ordinary Norn1 still
    // refuses the normalization collision.
    let profile = json!({"é":"one","e\u{301}":"two"});
    let bytes = assert_legacy_bytes(&profile);
    assert!(parse_json_verbatim(bytes).is_ok());
    assert!(canonical_json_of(&profile).is_err());
}

#[test]
fn map_order_matches_the_active_backend_instead_of_normalizing_it() {
    let mut fields = Map::new();
    fields.insert("z".into(), json!(1));
    fields.insert("a".into(), json!(2));
    let value = Value::Object(fields);
    let bytes = assert_legacy_bytes(&value);
    // Insertion order is selected only by the existing serde_json feature.
    let expected: &[u8] = if value
        .as_object()
        .expect("map")
        .keys()
        .next()
        .map(String::as_str)
        == Some("z")
    {
        br#"{"z":1,"a":2}"#
    } else {
        br#"{"a":2,"z":1}"#
    };
    assert_eq!(bytes, expected);
    assert_eq!(
        canonical_json_of(&value).expect("Norn1"),
        br#"{"a":2,"z":1}"#
    );
}

#[test]
fn exact_large_typed_integers_and_number_profile_tokens_are_preserved() {
    let integers = [9_007_199_254_740_993_u128, 18_446_744_073_709_551_617];
    let bytes = assert_legacy_bytes(&integers);
    assert_eq!(bytes, b"[9007199254740993,18446744073709551617]");
    assert_eq!(
        parse_json_verbatim(&bytes).expect("exact integers"),
        parse_json_verbatim(b"[9007199254740993,18446744073709551617]")
            .expect("exact decimal meaning")
    );
    // Both builds keep their CURRENT typed representation. Default Serde JSON
    // rounds the last input through f64; arbitrary_precision retains its token.
    let source = "[1.50,1e+2,-0.0,9007199254740993,18446744073709551617]";
    let value: Value = serde_json::from_str(source).expect("numeric tokens");
    let bytes = assert_legacy_bytes(&value);
    let probe = serde_json::from_str::<Value>("1.50").expect("feature probe");
    let exact_tokens =
        serialize_json_verbatim(&probe).expect("feature probe serialization") == b"1.50";
    let expected = if exact_tokens {
        source
    } else {
        "[1.5,100.0,-0.0,9007199254740993,1.8446744073709552e+19]"
    };
    assert_eq!(bytes, expected.as_bytes());
    assert_eq!(
        parse_json_verbatim(&bytes).expect("serialized numbers"),
        parse_json_verbatim(expected).expect("existing numeric meaning")
    );
    assert_eq!(assert_legacy_bytes(&-0.0_f64), b"-0.0");
    assert_eq!(canonical_json_of(&-0.0_f64).expect("Norn1"), b"0");
}

#[test]
fn escapes_and_existing_serialization_errors_are_unchanged() {
    let text = "\"\\/\u{0000}\u{0008}\u{000c}\n\r\t\u{001f}é\u{2028}";
    let expected = "\"\\\"\\\\/\\u0000\\b\\f\\n\\r\\t\\u001fé\u{2028}\"";
    assert_eq!(assert_legacy_bytes(text), expected.as_bytes());
    let invalid_keys = std::collections::BTreeMap::from([(vec![1, 2], 3)]);
    let legacy = serde_json::to_vec(&invalid_keys).expect_err("non-string map key");
    assert_eq!(
        serialize_json_verbatim(&invalid_keys),
        Err(CanonicalJsonError::Serialize {
            message: legacy.to_string()
        })
    );
}
