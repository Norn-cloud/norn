#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "integration-test assertions"
)]

use norn_ir::access::{AccessCondition, AccessLiteral};
use serde_json::json;

#[test]
fn long_literals_decode_from_integral_artifact_numbers_only() {
    for text in [
        "100",
        "100.0",
        "-100.0",
        "9223372036854775807",
        "-9223372036854775808",
    ] {
        assert!(matches!(
            serde_json::from_str::<AccessLiteral>(text).expect("Long"),
            AccessLiteral::Long(_)
        ));
    }
    for text in [
        "100.5",
        "9223372036854775808",
        "-9223372036854775809",
        "1e20",
    ] {
        assert!(serde_json::from_str::<AccessLiteral>(text).is_err());
    }
    assert_eq!(
        serde_json::from_str::<AccessLiteral>("true").expect("Bool"),
        AccessLiteral::Bool(true)
    );
    assert_eq!(
        serde_json::from_str::<AccessLiteral>(r#""100""#).expect("String"),
        AccessLiteral::String("100".to_owned())
    );
}

#[test]
fn resource_principal_operand_preserves_literals_and_rejects_open_objects() {
    let literal = json!({
        "resource": { "name": "business_unit", "op": "eq", "value": "sales" }
    });
    let decoded: AccessCondition = serde_json::from_value(literal.clone()).expect("literal");
    assert_eq!(serde_json::to_value(decoded).expect("serialize"), literal);

    let principal = json!({
        "resource": {
            "name": "business_unit",
            "op": "in",
            "value": { "principalAttribute": "business_units" }
        }
    });
    let decoded: AccessCondition =
        serde_json::from_value(principal.clone()).expect("principal reference");
    assert_eq!(serde_json::to_value(decoded).expect("serialize"), principal);

    for invalid in [
        json!({ "resource": { "name": "business_unit", "op": "eq", "value": { "principalAttribute": "units", "expression": "true" } } }),
        json!({ "context": { "name": "mfa", "op": "in", "value": true } }),
    ] {
        assert!(serde_json::from_value::<AccessCondition>(invalid).is_err());
    }
}
