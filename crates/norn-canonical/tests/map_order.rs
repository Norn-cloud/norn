//! #2882: canonical bytes and digests do not depend on `serde_json`'s map
//! backend. CI runs this crate with `serde_json/preserve_order` off (the
//! default `-p norn-canonical` build) and on (`--features
//! serde_json/preserve_order`, as feature-unified by `cedar-policy`); the
//! pinned values must hold in both.
#![allow(clippy::expect_used, reason = "integration-test crate")]

use norn_canonical::{canonical_json_of, domain_hash_hex, legacy_hash_value};
use serde_json::{Map, Value, json};

/// Keys inserted in reverse order at every level: under `preserve_order` a raw
/// `serde_json::to_vec` would emit `b` before `a`.
fn reversed() -> Value {
    let mut inner = Map::new();
    inner.insert("z".to_owned(), json!([true, "x"]));
    inner.insert("y".to_owned(), json!(1));
    let mut outer = Map::new();
    outer.insert("b".to_owned(), json!(0.5));
    outer.insert("a".to_owned(), Value::Object(inner));
    Value::Object(outer)
}

const CANONICAL: &str = r#"{"a":{"y":1,"z":[true,"x"]},"b":0.5}"#;

#[test]
fn canonical_json_of_sorts_keys_regardless_of_insertion_order() {
    let bytes = canonical_json_of(&reversed()).expect("canonical");
    assert_eq!(String::from_utf8(bytes).expect("utf-8"), CANONICAL);
}

#[test]
fn domain_hash_is_pinned() {
    assert_eq!(
        domain_hash_hex(b"norn.test.v1\0", &reversed()).expect("hash"),
        "3ccb10ae78f45fa2830fa812a51640d3f4086555db35644d2ea8027a025bf6f0"
    );
}

#[test]
fn legacy_hash_is_insertion_order_independent() {
    let parsed: Value = serde_json::from_str(CANONICAL).expect("parses");
    assert_eq!(
        legacy_hash_value(&reversed()).expect("hash"),
        legacy_hash_value(&parsed).expect("hash")
    );
}
