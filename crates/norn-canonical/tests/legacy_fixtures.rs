//! Recorded Legacy0 artifact vectors from the pre-registry hashing implementation.

use norn_canonical::HashProfile;

const BAC_ARTIFACT: &str = r#"{
    "irVersion":"0.1",
    "artifactId":"retail.entity.sku",
    "kind":"entity",
    "namespace":"retail",
    "name":"Sku",
    "version":"0.1.0",
    "lifecycle":"approved",
    "scope":{"orgId":"org_demo","workspaceId":"workspace_demo","environmentId":"sandbox"},
    "dependencies":[],
    "provenance":{"source":"agent","actorId":"agent_1"},
    "payload":{"fields":{"sku":{"required":true,"type":"text"}}}
}"#;

const PRODUCT_ARTIFACT: &str = r#"{"irVersion":"0.1","artifactId":"retail.Product.entity","kind":"entity","namespace":"retail","name":"Product","version":"1.0.0","lifecycle":"draft","scope":{"orgId":"org_v0_process","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk","sourcePath":"fixtures/v0/entities/product.artifact.json","actorId":"v0_process_author","createdAt":"2026-08-26T00:00:00Z"},"payload":{"objectKind":"retail.Product","fields":{"sku":{"type":"text","required":true},"title":{"type":"text","required":true},"category":{"type":"text","required":true},"price":{"type":"number","required":true},"currency":{"type":"enum","required":true,"values":["EGP"]},"stock":{"type":"integer","required":true}}}}"#;

const ORDER_ARTIFACT: &str = r#"{"irVersion":"0.1","artifactId":"retail.Order.entity","kind":"entity","namespace":"retail","name":"Order","version":"1.0.0","lifecycle":"draft","scope":{"orgId":"org_v0_process","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk","sourcePath":"fixtures/v0/entities/order.artifact.json","actorId":"v0_process_author","createdAt":"2026-08-26T00:00:00Z"},"payload":{"objectKind":"retail.Order","fields":{"orderId":{"type":"text","required":true},"customerName":{"type":"text","required":true},"channel":{"type":"enum","required":true,"values":["online","instagram","storefront","marketplace"]},"sku":{"type":"text","required":true},"qty":{"type":"integer","required":true},"total":{"type":"number","required":true},"status":{"type":"enum","required":true,"values":["pending","paid","packed","shipped","cancelled"]},"placedAt":{"type":"text","required":true}}}}"#;

const PROCESS_ARTIFACT: &str = r#"{"irVersion":"0.1","artifactId":"retail.order_fulfillment.process","kind":"process","namespace":"retail","name":"Order Fulfillment","version":"1.0.0","lifecycle":"draft","scope":{"orgId":"org_v0_process","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk","sourcePath":"fixtures/v0/order_fulfillment.process.json","actorId":"v0_process_author","createdAt":"2026-08-26T00:00:00Z"},"payload":{"processVersion":"1.0","processId":"order_fulfillment","graphVersion":"1.0","entryStepId":"pick","triggers":[{"triggerId":"manual_start","kind":"manual","entryStepId":"pick","scopeRequired":["orgId","environmentId"]}],"steps":[{"stepId":"pick","kind":"task","executorClass":"service","assignment":{"kind":"queue","target":"warehouse_pick"},"input":{"orderId":{"kind":"template","template":"{{event.orderId}}"},"sku":{"kind":"template","template":"{{event.sku}}"},"qty":{"kind":"template","template":"{{event.qty}}"}},"inputSchema":{"type":"object","properties":{"orderId":{"type":"string"},"sku":{"type":"string"},"qty":{"type":"integer"}},"required":["orderId","sku","qty"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"picked":{"type":"boolean"}},"required":["picked"],"additionalProperties":false},"requiredCapabilities":["order_fulfillment.pick"],"leasePolicy":{"leaseSeconds":900,"maxLeaseExtensions":1},"deadlineSeconds":3600,"transition":{"kind":"step","targetStepId":"pack"}},{"stepId":"pack","kind":"task","executorClass":"service","assignment":{"kind":"queue","target":"warehouse_pack"},"input":{"orderId":{"kind":"template","template":"{{event.orderId}}"},"sku":{"kind":"template","template":"{{event.sku}}"},"qty":{"kind":"template","template":"{{event.qty}}"}},"inputSchema":{"type":"object","properties":{"orderId":{"type":"string"},"sku":{"type":"string"},"qty":{"type":"integer"}},"required":["orderId","sku","qty"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"packed":{"type":"boolean"}},"required":["packed"],"additionalProperties":false},"requiredCapabilities":["order_fulfillment.pack"],"leasePolicy":{"leaseSeconds":900,"maxLeaseExtensions":1},"deadlineSeconds":3600,"transition":{"kind":"step","targetStepId":"payment_confirmed"}},{"stepId":"payment_confirmed","kind":"wait_signal","signalType":"payment_confirmed","correlationKey":{"kind":"template","template":"{{event.orderId}}"},"transition":{"kind":"step","targetStepId":"ship"}},{"stepId":"ship","kind":"task","executorClass":"service","assignment":{"kind":"queue","target":"warehouse_ship"},"input":{"orderId":{"kind":"template","template":"{{event.orderId}}"},"sku":{"kind":"template","template":"{{event.sku}}"},"qty":{"kind":"template","template":"{{event.qty}}"}},"inputSchema":{"type":"object","properties":{"orderId":{"type":"string"},"sku":{"type":"string"},"qty":{"type":"integer"}},"required":["orderId","sku","qty"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"shipped":{"type":"boolean"}},"required":["shipped"],"additionalProperties":false},"requiredCapabilities":["order_fulfillment.ship"],"leasePolicy":{"leaseSeconds":900,"maxLeaseExtensions":1},"deadlineSeconds":3600,"transition":{"kind":"end","status":"completed"}}],"references":[],"retryPolicy":{"maxAttempts":3,"retryDelaySeconds":0},"governance":{"scope":{"orgId":"org_v0_process","environmentId":"dev"},"publicationStatus":"active","ownerActorId":"v0_process_author","riskTier":"low","capabilityFootprint":["capability:order_fulfillment.pack","capability:order_fulfillment.pick","capability:order_fulfillment.ship","signal:payment_confirmed"],"reviewPolicyId":"order_fulfillment_review"}}}"#;

#[test]
fn legacy0_matches_recorded_artifact_hashes() {
    let active_process =
        PROCESS_ARTIFACT.replace(r#""lifecycle":"draft""#, r#""lifecycle":"active""#);
    let cases = [
        ("legacy BAC artifact", BAC_ARTIFACT),
        ("legacy Product artifact", PRODUCT_ARTIFACT),
        ("legacy Order artifact", ORDER_ARTIFACT),
        ("legacy process artifact", PROCESS_ARTIFACT),
        ("legacy active process artifact", active_process.as_str()),
    ];

    let expected = [
        "026f956e837ad090c704c1aca9211a2c4d859bdb14d1874c49f21d5ce648fca5",
        "3a811374bb9c5b9e19d45001f42ea0fd275c7c265e8836707e466414645234b1",
        "80da3e6475f7c3b58554f70815916a0d35ac734156ef949a5cba860b28934d7b",
        "230bcd0973cf0d8f036675fd0169a6bfabcf72a52b16ae2b652c1c6635e8b1cc",
        "bf52fd3d75fcb37eeb4c30e851e110e5b3b09ce04efc546b9a6b0719fd4e197e",
    ];
    for ((source, input), expected) in cases.iter().zip(expected) {
        assert_eq!(
            HashProfile::Legacy0.hash_json(input).ok(),
            Some(expected.to_owned()),
            "fixture source: {source}"
        );
    }
}

/// Legacy0 bytes must not depend on `serde_json/arbitrary_precision` (#2440):
/// that feature keeps number literals verbatim, the default profile re-renders
/// them from binary. CI runs this under both feature sets against one pin.
#[test]
fn legacy0_number_rendering_is_independent_of_serde_json_features() {
    let input = r#"{"z":[1.50,1e2,1E+2,-0,-0.0,0.0,10,0.1,9007199254740993,18446744073709551616,-7,1e-3,0.30000000000000004441,914.17e58]}"#;
    let expected = r#"{"z":[1.5,100.0,100.0,0,0,0.0,10,0.1,9007199254740993,1.8446744073709552e+19,-7,0.001,0.30000000000000004,9.141700000000001e+60]}"#;
    let canonical = HashProfile::Legacy0
        .canonicalize_json(input)
        .expect("numbers should canonicalize");
    assert_eq!(String::from_utf8(canonical).ok().as_deref(), Some(expected));
}

/// Sol review of #2440: numbers constructed in memory keep their bits in
/// Legacy0 under both `serde_json` profiles. Under `arbitrary_precision` a
/// constructed number is stored as its shortest rendering; re-parsing that with
/// the historical parser would move near-halfway values by one bit. CI runs
/// this in both the `test` and `number-profile` jobs.
#[test]
fn legacy0_keeps_constructed_float_bits_under_both_profiles() {
    let values = [
        9.1417e60,
        9.141_700_000_000_001e60,
        5e-324,
        2.225_073_858_507_201e-308,
        1.797_693_134_862_315_7e308,
        0.1 + 0.2,
        123_456_789.123_456_78,
    ];
    let array = values
        .iter()
        .map(|value| {
            serde_json::Number::from_f64(*value)
                .map(serde_json::Value::Number)
                .expect("finite")
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({ "z": array });
    let canonical = norn_canonical::legacy_canonicalize_value(&value).expect("canonicalize");
    let canonical_text = String::from_utf8(canonical).expect("utf8");
    // The rendered numbers round-trip (correctly rounded parse) to the
    // constructed bits, not to a historical-parser neighbour.
    let rendered = canonical_text
        .trim_start_matches(r#"{"z":["#)
        .trim_end_matches("]}")
        .split(',')
        .map(|text| text.parse::<f64>().expect("number"))
        .collect::<Vec<_>>();
    assert_eq!(
        rendered
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        canonical_text,
        r#"{"z":[9.1417e+60,9.141700000000001e+60,5e-324,2.225073858507201e-308,1.7976931348623157e+308,0.30000000000000004,123456789.12345678]}"#
    );
}
