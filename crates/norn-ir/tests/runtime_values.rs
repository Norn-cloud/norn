#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! #2770 (T3-NUM-1): exact runtime values. Decimals are the engine's 96-bit
//! decimal (scale <= 28), parsed from token text, refused rather than
//! rounded, written back as exact number tokens, and hashed as Norn1.
//! The golden corpus (`tests/golden/runtime-decimals.json`) pins canonical
//! forms and refusals; `wasm32` CI builds this crate, and the checker-wasm
//! parity lane can replay the same corpus.

use norn_ir::{Decimal, RuntimeObject, RuntimeValue, RuntimeValueError};
use serde_json::Value;

fn dec(text: &str) -> Decimal {
    Decimal::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn golden() -> Value {
    serde_json::from_str(include_str!("golden/runtime-decimals.json")).expect("golden corpus")
}

#[test]
fn golden_decimals_are_exact_and_canonical() {
    let corpus = golden();
    for case in corpus["exact"].as_array().expect("exact cases") {
        let input = case["input"].as_str().expect("input");
        let canonical = case["canonical"].as_str().expect("canonical");
        assert_eq!(dec(input).canonical(), canonical, "{input}");
    }
    for case in corpus["inexact"].as_array().expect("inexact cases") {
        let input = case.as_str().expect("input");
        assert!(
            matches!(
                Decimal::parse(input),
                Err(RuntimeValueError::Inexact { .. })
            ),
            "{input}: {:?}",
            Decimal::parse(input)
        );
    }
    for case in corpus["invalid"].as_array().expect("invalid cases") {
        let input = case.as_str().expect("input");
        assert!(
            matches!(
                Decimal::parse(input),
                Err(RuntimeValueError::InvalidJson { .. })
            ),
            "{input}: {:?}",
            Decimal::parse(input)
        );
    }
}

#[test]
fn equality_and_order_are_numeric() {
    assert_eq!(dec("1.50"), dec("1.5"));
    assert!(dec("2") > dec("1.99"));
    assert_eq!(dec("100").to_i64(), Some(100));
    assert_eq!(dec("1.5").to_i64(), None);
    assert_eq!(dec("12.340").scale(), 2);
    assert_eq!(Decimal::from(u64::MAX).canonical(), "18446744073709551615");
}

#[test]
fn json_text_round_trips_exactly() {
    let text = r#"{"amount":12.50,"big":9007199254740993,"list":[0.1,-0],"name":"Café","nested":{"t":true,"n":null}}"#;
    let value = RuntimeValue::from_json_text(text).expect("parses");
    let serde: RuntimeValue = serde_json::from_str(text).expect("serde parses");
    assert_eq!(value, serde);
    let written = serde_json::to_string(&value).expect("writes");
    assert_eq!(
        written,
        r#"{"amount":12.5,"big":9007199254740993,"list":[0.1,0],"name":"Café","nested":{"n":null,"t":true}}"#
    );
    assert_eq!(
        RuntimeValue::from_json_text(&written).expect("re-parses"),
        value
    );
    // Strings are kept verbatim (a decomposed "é" stays decomposed).
    let decomposed = RuntimeValue::from_json_text("\"Cafe\u{301}\"").expect("parses");
    assert_eq!(decomposed.as_str(), Some("Cafe\u{301}"));
}

#[test]
fn nested_in_typed_data_and_in_memory_values() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Fact {
        values: RuntimeObject,
        total: Decimal,
    }
    let fact: Fact = serde_json::from_str(
        r#"{"values":{"price":0.3000000000000000000000000001},"total":100.10}"#,
    )
    .expect("decodes");
    assert_eq!(fact.total, dec("100.1"));
    assert_eq!(
        fact.values.get("price"),
        Some(&RuntimeValue::Number(dec("0.3000000000000000000000000001")))
    );
    let text = serde_json::to_string(&fact).expect("encodes");
    assert_eq!(
        serde_json::from_str::<Fact>(&text).expect("re-decodes"),
        fact
    );
    // An inexact number inside typed data is a decode error, not a rounding.
    assert!(serde_json::from_str::<Fact>(r#"{"values":{},"total":1e40}"#).is_err());
    // From an in-memory `serde_json::Value`: integers exact, floats by their
    // shortest round-trip text.
    let from_value: RuntimeValue =
        serde_json::from_value(serde_json::json!({ "a": 1, "b": 0.1 })).expect("decodes");
    assert_eq!(
        from_value,
        RuntimeValue::Object(RuntimeObject::from([
            ("a".to_owned(), RuntimeValue::from(1)),
            ("b".to_owned(), RuntimeValue::Number(dec("0.1"))),
        ]))
    );
}

#[test]
fn hashing_is_norn1_over_exact_decimals() {
    let text = r#"{"b":1.50,"a":"x","c":0.3000000000000000000000000001}"#;
    let value = RuntimeValue::from_json_text(text).expect("parses");
    let canonical = value.canonical_json().expect("canonical");
    assert_eq!(
        canonical,
        br#"{"a":"x","b":1.5,"c":0.3000000000000000000000000001}"#
    );
    // The same bytes as Norn1 over the text, and as the generic
    // `canonical_json_of` over the serde form.
    assert_eq!(
        canonical,
        norn_ir::canonical::canonicalize_json(text).expect("norn1")
    );
    assert_eq!(
        canonical,
        norn_ir::canonical::canonical_json_of(&value).expect("serde")
    );
    // Pinned Norn1 bytes and hash of the golden document (native and wasm32
    // must agree).
    let corpus = golden();
    let value = RuntimeValue::from_json_text(include_str!("golden/runtime-decimals-document.json"))
        .expect("parses");
    let canonical = value.canonical_json().expect("canonical");
    assert_eq!(
        canonical,
        corpus["hashed"]["norn1Canonical"]
            .as_str()
            .expect("pinned bytes")
            .as_bytes()
    );
    assert_eq!(
        norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&canonical),
        corpus["hashed"]["norn1Sha256"]
            .as_str()
            .expect("pinned hash")
    );
}

#[test]
fn ir_numbers_convert_by_shortest_text() {
    assert_eq!(
        RuntimeValue::from_ir(&norn_ir::JsonValue::Number(0.1)).expect("converts"),
        RuntimeValue::Number(dec("0.1"))
    );
    assert!(RuntimeValue::from_ir(&norn_ir::JsonValue::Number(f64::NAN)).is_err());
    assert!(RuntimeValue::from_ir(&norn_ir::JsonValue::Number(1e30)).is_err());
}

#[test]
fn an_in_memory_serde_json_value_never_silently_rounds() {
    // Text serialization is exact; a `serde_json::Value` cannot hold every
    // decimal, so building one fails loudly instead of rounding.
    let exact = RuntimeValue::from_json_text(r#"{"price":0.3000000000000000000000000001}"#)
        .expect("parses");
    assert!(serde_json::to_value(&exact).is_err());
    assert_eq!(
        serde_json::to_string(&exact).expect("text"),
        r#"{"price":0.3000000000000000000000000001}"#
    );
    // Decimals that `f64` holds exactly (in JSON terms) convert.
    let short =
        RuntimeValue::from_json_text(r#"{"price":0.3,"big":9007199254740992}"#).expect("parses");
    let value = serde_json::to_value(&short).expect("converts");
    assert_eq!(
        value["price"].as_f64().map(f64::to_bits),
        Some(0.3_f64.to_bits())
    );
    assert_eq!(
        serde_json::from_value::<RuntimeValue>(value).expect("back"),
        short
    );
    // Integers in u64 range are exact `serde_json` integers; beyond, refused.
    let integer = RuntimeValue::from_json_text("9007199254740993").expect("parses");
    assert_eq!(
        serde_json::to_value(&integer).expect("u64").as_u64(),
        Some(9_007_199_254_740_993)
    );
    let huge = RuntimeValue::from_json_text("79228162514264337593543950335").expect("parses");
    assert!(serde_json::to_value(&huge).is_err());
}

#[test]
fn binary_conversion_never_reparses_through_serde_json() {
    // `0.9999999999999999` is an exact f64 shortest text; serde_json's own
    // default float parser would read its raw token as 1.0.
    let value = RuntimeValue::from_json_text(
        r#"{"x":0.9999999999999999,"list":[0.9999999999999999],"nested":{"y":0.9999999999999999}}"#,
    )
    .expect("parses");
    let expected = "0.9999999999999999".parse::<f64>().expect("f64").to_bits();
    let bits = |converted: &serde_json::Value, pointer: &str| {
        converted
            .pointer(pointer)
            .and_then(serde_json::Value::as_f64)
            .map(f64::to_bits)
    };
    let converted = serde_json::to_value(&value).expect("converts");
    for pointer in ["/x", "/list/0", "/nested/y"] {
        assert_eq!(bits(&converted, pointer), Some(expected), "{pointer}");
    }
    let map: std::collections::BTreeMap<String, RuntimeValue> =
        std::collections::BTreeMap::from([("z".to_owned(), value)]);
    let converted = serde_json::to_value(&map).expect("converts");
    assert_eq!(bits(&converted, "/z/x"), Some(expected));
}

#[test]
fn fact_changes_decode_exactly_and_refuse_foreign_members_even_when_null() {
    use norn_ir::FactChange;
    let set: FactChange = serde_json::from_str(
        r#"{"op":"set","objectType":"T","key":"1","property":"p","value":0.3000000000000000000000000001}"#,
    )
    .expect("decodes");
    let FactChange::Set { value, .. } = set else {
        panic!("a set");
    };
    assert_eq!(
        value,
        RuntimeValue::Number(dec("0.3000000000000000000000000001"))
    );
    // A present `null` value is a value.
    let null: FactChange = serde_json::from_str(
        r#"{"op":"set","objectType":"T","key":"1","property":"p","value":null}"#,
    )
    .expect("decodes");
    let FactChange::Set {
        value: null_value, ..
    } = null
    else {
        panic!("a set");
    };
    assert_eq!(null_value, RuntimeValue::Null);
    // A member of another op is unknown, also as `null`; a missing one is
    // missing; a wrong type is refused.
    for refused in [
        r#"{"op":"delete","objectType":"T","key":"1","values":null}"#,
        r#"{"op":"create","objectType":"T","key":"1"}"#,
        r#"{"op":"create","objectType":"T","key":"1","values":null}"#,
        r#"{"op":"transition","objectType":"T","key":"1","lifecycle":"s","transition":1}"#,
        r#"{"op":"teleport","objectType":"T","key":"1"}"#,
        r#"{"op":"delete","objectType":"T","key":"1","extra":1}"#,
    ] {
        assert!(
            serde_json::from_str::<FactChange>(refused).is_err(),
            "{refused}"
        );
    }
}
