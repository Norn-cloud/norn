#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! The cell ABI natively (T4-CELL-4, #2678): plans, verdicts, refusals, and
//! exact payload bytes (T3-NUM-1, #2770).

use norn_cell_wasm::{
    AbiError, MAX_SAFE_INTEGER, check_commit_json, decode_commit_json, decode_observed_json,
    resolve_key_json, validate_input_json, version_json,
};
use norn_testkit::cell_host::law_object_types;
use serde_json::{Value, json};

/// The law types as the ABI's model argument (#3018).
fn model() -> String {
    serde_json::to_string(&law_object_types()).expect("model")
}

fn owned_rows_model() -> String {
    json!({
        "objectTypes": [
            {
                "name": "abi.Order",
                "properties": [{ "id": "title", "type": { "kind": "text" } }],
                "links": [{
                    "id": "items",
                    "target": "abi.Line",
                    "cardinality": { "to": "many", "from": "one" },
                    "inverse": "parent"
                }],
                "ownedCollections": [{
                    "id": "items",
                    "childType": "abi.Line",
                    "rowIdProperty": "row_id",
                    "orderingKey": "idx",
                    "cascadeOnParentDelete": true,
                    "maxRows": 2
                }]
            },
            {
                "name": "abi.Line",
                "properties": [
                    { "id": "row_id", "type": { "kind": "text" } },
                    { "id": "idx", "type": { "kind": "integer" } }
                ]
            },
            { "name": "norn.OwnedRows" },
            {
                "name": "norn.ActionCall",
                "properties": [{ "id": "effectKind", "type": { "kind": "text" } }]
            }
        ],
        "interfaces": []
    })
    .to_string()
}

/// A view: per plan object `(version, live concrete type)`, no live links,
/// and the plan links' versions.
fn view(seq: u64, pending: bool, objects: &[(u64, Option<&str>)], links: &[u64]) -> String {
    let objects: Vec<Value> = objects
        .iter()
        .map(|(version, concrete)| json!({ "version": version, "concrete": concrete, "links": [] }))
        .collect();
    json!({ "seq": seq, "pending": pending, "objects": objects, "linkVersions": links, "linkReads": [] }).to_string()
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).expect("ABI output is JSON")
}

fn key(n: u8) -> Value {
    json!({ "objectType": "law.Thing", "key": format!("k{n}") })
}

/// Number tokens that an `f64` or a JavaScript number would change: the
/// shim's T3-NUM-1 goldens (`apps/cell-shim/test/fixtures/numeric-goldens.json`).
fn numeric_goldens() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/cell-shim/test/fixtures/numeric-goldens.json");
    let text = std::fs::read_to_string(&path).expect("read numeric-goldens.json");
    let goldens: Value = serde_json::from_str(&text).expect("numeric-goldens.json parses");
    goldens["tokens"]
        .as_array()
        .expect("tokens array")
        .iter()
        .map(|token| token.as_str().expect("token string").to_owned())
        .collect()
}

fn golden_commit(token: &str) -> String {
    format!(
        concat!(
            r#"{{"consumes":null,"reads":[],"#,
            r#""writes":[{{"key":{{"objectType":"law.Thing","key":"k0"}},"expected":0,"mutation":{{"put":{{"values":{{"amount":{t},"nested":[{t}]}}}}}}}}],"#,
            r#""events":[{t},{{"amount":{t}}}],"#,
            r#""effects":["#,
            r#"{{"perform":{{"id":"c0","generation":1,"idempotency_key":"c0/1","freshness":"append_only","deadline":null,"payload":{t}}}}},"#,
            r#"{{"schedule":{{"id":"t0","due":5,"payload":{{"amount":{t}}}}}}}"#,
            r#"]}}"#
        ),
        t = token
    )
}

#[test]
fn payload_numbers_cross_the_abi_as_exact_bytes() {
    let tokens = numeric_goldens();
    assert!(tokens.len() >= 8, "numeric goldens missing");
    for token in &tokens {
        let commit = golden_commit(token);
        let plan = parse(&decode_commit_json(&commit, &model()).expect("plan"));
        assert_eq!(
            plan["writes"][0]["values"],
            json!(format!(r#"{{"amount":{token},"nested":[{token}]}}"#)),
            "{token}"
        );
        let command = plan["effects"][0]["command"]
            .as_str()
            .expect("command text");
        assert!(
            command.ends_with(&format!(r#""payload":{token}}}"#)),
            "{command}"
        );
        let timer = plan["effects"][1]["timer"].as_str().expect("timer text");
        assert!(
            timer.contains(&format!(r#"{{"amount":{token}}}"#)),
            "{timer}"
        );

        let verdict = parse(
            &check_commit_json(&commit, &view(0, false, &[(0, None)], &[]), &model())
                .expect("verdict"),
        );
        assert_eq!(verdict["accepted"], json!(true));
        let entry = verdict["entry"].as_str().expect("entry text");
        assert!(
            entry.ends_with(&format!(r#""events":[{token},{{"amount":{token}}}]}}"#)),
            "{entry}"
        );
    }
}

#[test]
fn unrelated_invariants_do_not_decode_opaque_put_values() {
    let model = json!({
        "objectTypes": [
            { "name": "abi.Unconstrained" },
            {
                "name": "abi.Constrained",
                "invariants": [{
                    "id": "positive_amount",
                    "expression": "self.amount > 0",
                    "message": "amount must be positive"
                }]
            }
        ],
        "interfaces": []
    })
    .to_string();
    let commit = r#"{"consumes":null,"reads":[],"writes":[{"key":{"objectType":"abi.Unconstrained","key":"a1"},"expected":0,"mutation":{"put":{"values":{"amount":0.123456789012345678901234567890123456789}}}}],"events":[],"effects":[]}"#;

    let plan = parse(&decode_commit_json(commit, &model).expect("plan"));
    assert_eq!(
        plan["writes"][0]["values"],
        json!(r#"{"amount":0.123456789012345678901234567890123456789}"#)
    );

    let verdict = parse(
        &check_commit_json(
            commit,
            &json!({
                "seq": 0,
                "pending": false,
                "objects": [{ "version": 0, "concrete": null, "links": [] }],
                "linkVersions": [],
                "linkReads": []
            })
            .to_string(),
            &model,
        )
        .expect("verdict"),
    );
    assert_eq!(verdict["accepted"], json!(true));
}

#[test]
fn plan_lists_keys_writes_and_effects_in_order() {
    let commit = json!({
        "consumes": { "timer": { "id": "t1", "incarnation": 3 } },
        "reads": [{ "key": key(2), "expected": 4 }],
        "writes": [
            { "key": key(0), "expected": 0, "mutation": { "put": { "values": { "v": 1 } } } },
            { "key": key(1), "expected": 2, "mutation": "delete" }
        ],
        "events": [7],
        "effects": [
            { "perform": { "id": "c0", "generation": 2, "idempotency_key": "c0/2", "freshness": "append_only", "deadline": 9, "payload": 1 } },
            { "schedule": { "id": "t0", "due": 5, "payload": { "timer": 1 } } },
            { "cancel_timer": "t2" }
        ]
    });
    let plan = parse(&decode_commit_json(&commit.to_string(), &model()).expect("plan"));
    assert_eq!(plan["format"], json!("norn-cell-abi/plan@4"));
    assert_eq!(
        plan["consumes"],
        json!({ "kind": "timer", "id": "t1", "incarnation": 3 })
    );
    assert_eq!(plan["objects"], json!([key(2), key(0), key(1)]));
    assert_eq!(plan["writes"][0]["values"], json!(r#"{"v":1}"#));
    assert_eq!(plan["writes"][1]["values"], Value::Null);
    assert_eq!(plan["effects"][0]["kind"], json!("perform"));
    assert_eq!(plan["effects"][0]["id"], json!("c0"));
    assert_eq!(plan["effects"][0]["generation"], json!(2));
    assert_eq!(plan["effects"][0]["idempotencyKey"], json!("c0/2"));
    assert_eq!(plan["effects"][0]["deadline"], json!(9));
    assert_eq!(plan["effects"][1]["kind"], json!("schedule"));
    assert_eq!(plan["effects"][1]["due"], json!(5));
    assert_eq!(
        plan["effects"][2],
        json!({ "kind": "cancel_timer", "id": "t2" })
    );
}

fn simple(consumes: &Value, reads: &Value, writes: &Value) -> String {
    json!({ "consumes": consumes, "reads": reads, "writes": writes, "events": [], "effects": [] })
        .to_string()
}

fn put(n: u8, expected: u64) -> Value {
    json!({ "key": key(n), "expected": expected, "mutation": { "put": { "values": { "v": n } } } })
}

#[test]
fn verdicts_follow_the_kernel_check_order() {
    // Duplicate key before a not-pending delivery.
    let duplicate = simple(
        &json!({ "observation": { "id": "o1" } }),
        &json!([{ "key": key(0), "expected": 0 }]),
        &json!([put(0, 0)]),
    );
    let verdict = parse(
        &check_commit_json(&duplicate, &view(0, false, &[(0, None)], &[]), &model())
            .expect("verdict"),
    );
    assert_eq!(verdict["accepted"], json!(false));
    assert_eq!(verdict["refusal"]["code"], json!("commit.duplicate_key"));
    assert_eq!(verdict["refusal"]["key"], key(0));

    // Not pending before a version conflict.
    let stale = simple(
        &json!({ "observation": { "id": "o1" } }),
        &json!([]),
        &json!([put(0, 1)]),
    );
    let verdict = parse(
        &check_commit_json(&stale, &view(0, false, &[(0, None)], &[]), &model()).expect("verdict"),
    );
    assert_eq!(verdict["refusal"]["code"], json!("commit.not_pending"));
    assert_eq!(
        verdict["refusal"]["consumed"],
        json!({ "observation": { "id": "o1" } })
    );

    let verdict = parse(
        &check_commit_json(&stale, &view(0, true, &[(0, None)], &[]), &model()).expect("verdict"),
    );
    assert_eq!(verdict["refusal"]["code"], json!("commit.conflict"));
    assert_eq!(verdict["refusal"]["expected"], json!(1));
    assert_eq!(verdict["refusal"]["current"], json!(0));

    let verdict = parse(
        &check_commit_json(
            &stale,
            &view(6, true, &[(1, Some("law.Thing"))], &[]),
            &model(),
        )
        .expect("verdict"),
    );
    assert_eq!(verdict["accepted"], json!(true));
    assert_eq!(verdict["seq"], json!(7));
    assert_eq!(verdict["written"], json!([{ "version": 2 }]));
    assert_eq!(
        parse(verdict["receipt"].as_str().expect("receipt")),
        json!({ "seq": 7, "written": [{ "key": key(0), "version": 2 }] })
    );
    assert_eq!(
        parse(verdict["entry"].as_str().expect("entry")),
        json!({
            "seq": 7,
            "consumed": { "observation": { "id": "o1" } },
            "written": [{ "key": key(0), "version": 2 }],
            "linked": [],
            "events": []
        })
    );
}

#[test]
fn referenced_defaults_are_evaluated_and_included_in_the_storage_plan() {
    for default in [-1, 1] {
        let model = json!({
            "objectTypes": [{
                "name": "law.Thing",
                "properties": [{
                    "id": "amount",
                    "type": {"kind": "ref", "name": "abi.Amount"},
                    "default": default.to_string()
                }],
                "invariants": [{
                    "id": "nonnegative_amount",
                    "expression": "(self.amount ?? 0) >= 0",
                    "message": "amount must be nonnegative"
                }]
            }],
            "propertyTypes": [{"name": "abi.Amount", "type": {"kind": "integer"}}]
        })
        .to_string();
        let commit = json!({
            "consumes": null, "reads": [],
            "writes": [{"key": key(0), "expected": 0, "mutation": {"put": {"values": {}}}}],
            "events": [], "effects": []
        })
        .to_string();
        let plan = parse(&decode_commit_json(&commit, &model).expect("defaulted storage plan"));
        let values = parse(
            plan["writes"][0]["values"]
                .as_str()
                .expect("planned values text"),
        );
        assert_eq!(values, json!({"amount": default}));
        let verdict = parse(
            &check_commit_json(&commit, &view(0, false, &[(0, None)], &[]), &model)
                .expect("defaulted commit verdict"),
        );
        assert_eq!(verdict["accepted"], json!(default >= 0));

        let token = "0.123456789012345678901234567890123456789";
        let opaque = commit.replace(
            "\"values\":{}",
            &format!("\"values\":{{\"opaque\":{token}}}"),
        );
        let plan =
            parse(&decode_commit_json(&opaque, &model).expect("opaque values with defaults"));
        let values = plan["writes"][0]["values"]
            .as_str()
            .expect("planned opaque text");
        assert!(
            values.contains(token),
            "adding defaults must retain submitted number tokens"
        );
        assert!(values.contains(&format!("\"amount\":{default}")));
    }
}

#[test]
fn numeric_strings_are_checked_as_the_numbers_in_the_storage_plan() {
    for property_type in [
        json!({"kind": "integer"}),
        json!({"kind": "decimal", "scale": 2}),
    ] {
        let model = json!({
            "objectTypes": [{
                "name": "law.Thing",
                "properties": [{"id": "amount", "type": {"kind": "ref", "name": "abi.Amount"}}],
                "invariants": [{
                    "id": "not_negative_one", "expression": "self.amount != -1",
                    "message": "amount cannot be negative one"
                }]
            }],
            "propertyTypes": [{"name": "abi.Amount", "type": property_type}]
        })
        .to_string();
        for amount in [-1, 1] {
            let commit = json!({
                "consumes": null, "reads": [],
                "writes": [{"key": key(0), "expected": 0, "mutation": {"put": {
                    "values": {"amount": amount.to_string()}
                }}}],
                "events": [], "effects": []
            })
            .to_string();
            let plan = parse(&decode_commit_json(&commit, &model).expect("numeric storage plan"));
            let values = parse(
                plan["writes"][0]["values"]
                    .as_str()
                    .expect("planned values"),
            );
            assert_eq!(values, json!({"amount": amount}));
            let verdict = parse(
                &check_commit_json(&commit, &view(0, false, &[(0, None)], &[]), &model)
                    .expect("numeric verdict"),
            );
            assert_eq!(verdict["accepted"], json!(amount != -1));
            if amount == -1 {
                assert_eq!(
                    verdict["refusal"]["code"],
                    json!("commit.invariant_violation")
                );
            }
        }
    }
}

#[test]
fn narrowed_subtype_keeps_the_inherited_default_in_the_storage_plan() {
    let model = json!({"objectTypes": [{
        "name": "abi.Order",
        "properties": [{
            "id": "status", "type": {"kind": "enum", "values": [{"value": "draft"}, {"value": "approved"}]},
            "default": "draft"
        }],
        "invariants": [{"id": "draft_only", "expression": "self.status == \"draft\"", "message": "must be draft"}]
    }, {
        "name": "abi.QuickOrder", "extends": "abi.Order",
        "properties": [{"id": "status", "type": {"kind": "enum", "values": [{"value": "draft"}]}}]
    }]}).to_string();
    let commit = json!({
        "consumes": null, "reads": [],
        "writes": [{"key": {"objectType": "abi.QuickOrder", "key": "Q-1"}, "expected": 0,
                    "mutation": {"put": {"values": {}}}}],
        "events": [], "effects": []
    })
    .to_string();
    let plan = parse(&decode_commit_json(&commit, &model).expect("inherited default plan"));
    assert_eq!(
        parse(plan["writes"][0]["values"].as_str().expect("values")),
        json!({"status": "draft"})
    );
    let verdict = parse(
        &check_commit_json(&commit, &view(0, false, &[(0, None)], &[]), &model).expect("verdict"),
    );
    assert_eq!(verdict["accepted"], json!(true));
}

#[test]
fn shared_admission_uses_interface_invariants_and_exact_put_values() {
    let model = json!({
        "objectTypes": [{
            "name": "abi.Thing",
            "implements": ["abi.PositiveAmount"]
        }],
        "interfaces": [{
            "name": "abi.PositiveAmount",
            "invariants": [{
                "id": "positive_amount",
                "expression": "self.amount > 0",
                "message": "amount must be positive"
            }]
        }]
    })
    .to_string();
    let commit = r#"{"consumes":null,"reads":[],"writes":[{"key":{"objectType":"abi.Thing","key":"a1"},"expected":0,"mutation":{"put":{"values":{"amount":-0.0000000000000000000000000001}}}}],"events":[],"effects":[]}"#;
    let verdict = parse(
        &check_commit_json(
            commit,
            &json!({
                "seq": 0,
                "pending": false,
                "objects": [{ "version": 0, "concrete": null, "links": [] }],
                "linkVersions": [],
                "linkReads": []
            })
            .to_string(),
            &model,
        )
        .expect("verdict"),
    );
    assert_eq!(verdict["accepted"], json!(false));
    assert_eq!(
        verdict["refusal"]["code"],
        json!("commit.invariant_violation")
    );
    assert_eq!(
        verdict["refusal"]["invariantOwner"],
        json!({ "kind": "interface", "name": "abi.PositiveAmount" })
    );
    assert_eq!(verdict["refusal"]["invariantId"], json!("positive_amount"));
    assert_eq!(
        verdict["refusal"]["invariantMessage"],
        json!("amount must be positive")
    );

    // A decimal too large for the exact runtime profile is malformed input,
    // rather than silently rounded invariant data.
    let inexact = commit.replace("-0.0000000000000000000000000001", "1e100");
    assert!(matches!(
        check_commit_json(
            &inexact,
            &json!({
                "seq": 0,
                "pending": false,
                "objects": [{ "version": 0, "concrete": null, "links": [] }],
                "linkVersions": [],
                "linkReads": []
            })
            .to_string(),
            &model,
        ),
        Err(AbiError::InvalidInput {
            argument: "commit",
            ..
        })
    ));
}

#[test]
fn invalid_input_is_an_error_not_a_refusal() {
    let empty = simple(&Value::Null, &json!([]), &json!([]));
    let views = [
        r#"{"seq":0,"pending":false,"objects":[],"linkVersions":[],"linkReads":[]}"#,
        r#"{"seq":0,"pending":false}"#,
        r#"{"seq":0,"pending":false,"objects":[{"version":1,"concrete":null,"links":[]}],"linkVersions":[],"linkReads":[]}"#,
        r#"{"seq":0,"pending":false,"objects":[],"linkVersions":[],"linkReads":[],"extra":1}"#,
    ];
    for (commit, view) in [
        ("not json", views[0]),
        (empty.as_str(), views[1]),
        (empty.as_str(), views[2]),
        (empty.as_str(), views[3]),
    ] {
        assert!(
            matches!(
                check_commit_json(commit, view, &model()),
                Err(AbiError::InvalidInput { .. })
            ),
            "{commit} / {view}"
        );
    }
    assert!(matches!(
        check_commit_json(&empty, views[0], "not a model"),
        Err(AbiError::InvalidInput { .. })
    ));
    let unknown = r#"{"consumes":null,"reads":[],"writes":[],"events":[],"effects":[],"x":1}"#;
    assert!(matches!(
        decode_commit_json(unknown, &model()),
        Err(AbiError::InvalidInput { .. })
    ));
    let not_object = simple(
        &Value::Null,
        &json!([]),
        &json!([{ "key": key(0), "expected": 0, "mutation": { "put": { "values": [1] } } }]),
    );
    assert!(matches!(
        decode_commit_json(&not_object, &model()),
        Err(AbiError::InvalidInput { .. })
    ));
    let duplicate_property = r#"{"consumes":null,"reads":[],"writes":[{"key":{"objectType":"law.Thing","key":"k0"},"expected":0,"mutation":{"put":{"values":{"v":1,"v":2}}}}],"events":[],"effects":[]}"#;
    assert!(matches!(
        decode_commit_json(duplicate_property, &model()),
        Err(AbiError::InvalidInput { .. })
    ));
}

#[test]
fn integers_beyond_javascript_precision_are_refused() {
    let unsafe_version: u64 = 9_007_199_254_740_992;
    assert!(unsafe_version > MAX_SAFE_INTEGER);
    let commit = simple(&Value::Null, &json!([]), &json!([put(0, unsafe_version)]));
    assert!(matches!(
        decode_commit_json(&commit, &model()),
        Err(AbiError::UnsafeInteger {
            field: "writes.expected",
            ..
        })
    ));
    let commit = simple(&Value::Null, &json!([]), &json!([]));
    let view = view(unsafe_version, false, &[], &[]);
    assert!(matches!(
        check_commit_json(&commit, &view, &model()),
        Err(AbiError::UnsafeInteger {
            field: "view.seq",
            ..
        })
    ));
}

#[test]
fn observations_need_provenance_and_inputs_must_be_one_value() {
    let observed = r#"{"id":"o1","provenance":{"source":"ERPNext","sourceId":"Employee/EMP1","observedAt":0,"mappingVersion":"erpnext/1"},"input":{"amount":9007199254740993}}"#;
    assert_eq!(
        parse(&decode_observed_json(observed).expect("observed")),
        json!({ "id": "o1" })
    );
    let missing = r#"{"id":"o1","input":1}"#;
    assert!(decode_observed_json(missing).is_err());
    assert!(validate_input_json(r#"{"result":18446744073709551615}"#).is_ok());
    assert!(validate_input_json("1 2").is_err());
    assert!(validate_input_json("").is_err());
}

#[test]
fn version_names_the_abi() {
    let version = parse(&version_json());
    assert_eq!(version["abi"], json!("norn-cell-abi/4"));
    assert_eq!(version["planFormat"], json!("norn-cell-abi/plan@4"));
    assert_eq!(version["verdictFormat"], json!("norn-cell-abi/verdict@4"));
    assert_eq!(version["maxSafeInteger"], json!(MAX_SAFE_INTEGER));
}

fn link_key(source: u8, link: &str, target: u8) -> Value {
    json!({ "source": key(source), "link": link, "target": key(target) })
}

fn link_read_view(targets: &Value) -> String {
    json!({
        "seq": 0,
        "pending": false,
        "objects": [],
        "linkVersions": [],
        "linkReads": [targets]
    })
    .to_string()
}

fn link_read_commit(targets: &Value) -> String {
    json!({
        "consumes": null,
        "reads": [],
        "writes": [],
        "events": [],
        "effects": [],
        "link_reads": [{
            "source": key(0),
            "link": "rel",
            "targets": targets
        }]
    })
    .to_string()
}

#[test]
fn link_read_plan_and_exact_stale_snapshots_are_checked() {
    let expected_targets = json!([
        { "key": link_key(0, "rel", 1), "version": 3 }
    ]);
    let commit = link_read_commit(&expected_targets);

    let plan = parse(&decode_commit_json(&commit, &model()).expect("plan"));
    assert_eq!(plan["format"], json!("norn-cell-abi/plan@4"));
    assert_eq!(plan["objects"], json!([]));
    assert_eq!(
        plan["linkReads"],
        json!([{
            "source": key(0),
            "link": "rel",
            "maxRecords": 1000,
            "maxCanonicalBytes": 262_144
        }])
    );

    let passed = parse(
        &check_commit_json(&commit, &link_read_view(&expected_targets), &model()).expect("verdict"),
    );
    assert_eq!(passed["accepted"], json!(true));

    let stale_version = json!([
        { "key": link_key(0, "rel", 1), "version": 4 }
    ]);
    let stale = parse(
        &check_commit_json(&commit, &link_read_view(&stale_version), &model()).expect("verdict"),
    );
    assert_eq!(stale["refusal"]["code"], json!("commit.link_read_conflict"));

    let phantom_target = json!([
        { "key": link_key(0, "rel", 1), "version": 3 },
        { "key": link_key(0, "rel", 2), "version": 1 }
    ]);
    let phantom = parse(
        &check_commit_json(&commit, &link_read_view(&phantom_target), &model()).expect("verdict"),
    );
    assert_eq!(
        phantom["refusal"]["code"],
        json!("commit.link_read_conflict")
    );

    let unavailable = parse(
        &check_commit_json(&commit, &link_read_view(&Value::Null), &model()).expect("verdict"),
    );
    assert_eq!(
        unavailable["refusal"]["code"],
        json!("commit.unsupported_link_read")
    );
}

#[test]
fn link_read_views_reject_wrong_sources_duplicates_and_unordered_targets() {
    let commit = link_read_commit(&json!([
        { "key": link_key(0, "rel", 1), "version": 3 }
    ]));
    let wrong_source = json!([
        { "key": link_key(2, "rel", 1), "version": 3 }
    ]);
    assert!(matches!(
        check_commit_json(&commit, &link_read_view(&wrong_source), &model()),
        Err(AbiError::InvalidInput {
            argument: "view",
            ..
        })
    ));

    let unsorted = json!([
        { "key": link_key(0, "rel", 2), "version": 1 },
        { "key": link_key(0, "rel", 1), "version": 3 }
    ]);
    assert!(matches!(
        check_commit_json(&commit, &link_read_view(&unsorted), &model()),
        Err(AbiError::InvalidInput {
            argument: "view",
            ..
        })
    ));

    let duplicate = json!([
        { "key": link_key(0, "rel", 1), "version": 3 },
        { "key": link_key(0, "rel", 1), "version": 3 }
    ]);
    assert!(matches!(
        check_commit_json(&commit, &link_read_view(&duplicate), &model()),
        Err(AbiError::InvalidInput {
            argument: "view",
            ..
        })
    ));
}

#[test]
fn link_read_views_enforce_target_count_and_canonical_byte_bounds() {
    let commit = link_read_commit(&json!([
        { "key": link_key(0, "rel", 1), "version": 3 }
    ]));
    let oversized = (0..1001)
        .map(|index| {
            json!({
                "key": {
                    "source": key(0),
                    "link": "rel",
                    "target": { "objectType": "law.Thing", "key": format!("t{index:04}") }
                },
                "version": 1
            })
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        check_commit_json(&commit, &link_read_view(&json!(oversized)), &model()),
        Err(AbiError::InvalidInput {
            argument: "view",
            ..
        })
    ));

    let over_budget = (0..513)
        .map(|index| {
            let target_key = format!("{index:04}") + &"k".repeat(508);
            json!({
                "key": {
                    "source": key(0),
                    "link": "rel",
                    "target": { "objectType": "law.Thing", "key": target_key }
                },
                "version": 1
            })
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        check_commit_json(&commit, &link_read_view(&json!(over_budget)), &model()),
        Err(AbiError::InvalidInput {
            argument: "view",
            ..
        })
    ));
}

#[test]
fn link_read_versions_must_be_safe_integers() {
    let commit = link_read_commit(&json!([
        { "key": link_key(0, "rel", 1), "version": 3 }
    ]));
    let unsafe_version = json!([
        { "key": link_key(0, "rel", 1), "version": MAX_SAFE_INTEGER + 1 }
    ]);
    assert!(matches!(
        check_commit_json(&commit, &link_read_view(&unsafe_version), &model()),
        Err(AbiError::UnsafeInteger {
            field: "view.linkReads.version",
            ..
        })
    ));

    let unsafe_expected = json!({
        "consumes": null,
        "reads": [],
        "writes": [],
        "events": [],
        "effects": [],
        "link_reads": [{
            "source": key(0),
            "link": "rel",
            "targets": [{ "key": link_key(0, "rel", 1), "version": MAX_SAFE_INTEGER + 1 }]
        }]
    })
    .to_string();
    assert!(matches!(
        decode_commit_json(&unsafe_expected, &model()),
        Err(AbiError::UnsafeInteger {
            field: "link_reads.targets.version",
            ..
        })
    ));
}

#[test]
fn link_read_view_requires_a_snapshot_for_each_declaration() {
    let empty_read_commit = json!({
        "consumes": null,
        "reads": [],
        "writes": [],
        "events": [],
        "effects": [],
        "link_reads": [{ "source": key(0), "link": "rel", "targets": [] }]
    })
    .to_string();
    let missing_snapshot = json!({
        "seq": 0,
        "pending": false,
        "objects": [],
        "linkVersions": [],
        "linkReads": []
    })
    .to_string();
    assert!(matches!(
        check_commit_json(&empty_read_commit, &missing_snapshot, &model()),
        Err(AbiError::InvalidInput {
            argument: "view",
            ..
        })
    ));
}

/// #3013/#3018: link writes cross the ABI with canonical keys; the verdict
/// runs the kernel's link checks against the model and the view.
#[test]
fn links_are_planned_and_checked_by_the_kernel() {
    let commit = json!({
        "consumes": null,
        "reads": [],
        "writes": [{ "key": key(0), "expected": 0, "mutation": { "put": { "values": { "v": 1 } } } }],
        "events": [],
        "effects": [],
        "links": [
            { "key": link_key(0, "rel", 1), "expected": 0, "mutation": { "add": { "values": { "w": 1.5 } } } },
            { "key": link_key(1, "rel", 1), "expected": 3, "mutation": "remove" }
        ]
    })
    .to_string();
    let plan = parse(&decode_commit_json(&commit, &model()).expect("plan"));
    assert_eq!(
        plan["links"],
        json!([
            { "link": link_key(0, "rel", 1), "values": r#"{"w":1.5}"# },
            { "link": link_key(1, "rel", 1), "values": null }
        ])
    );
    assert_eq!(plan["objects"], json!([key(0), key(1)]));
    let check = |k1: Option<&str>, links: &[u64]| {
        parse(
            &check_commit_json(
                &commit,
                &view(2, false, &[(0, None), (1, k1)], links),
                &model(),
            )
            .expect("verdict"),
        )
    };
    // k1 does not exist: the add would dangle.
    let verdict = check(None, &[0, 3]);
    assert_eq!(verdict["refusal"]["code"], json!("commit.dangling_link"));
    assert_eq!(verdict["refusal"]["missing"], key(1));
    let verdict = check(Some("law.Thing"), &[0, 2]);
    assert_eq!(verdict["refusal"]["code"], json!("commit.link_conflict"));
    assert_eq!(verdict["refusal"]["link"], link_key(1, "rel", 1));
    let verdict = check(Some("law.Thing"), &[0, 3]);
    assert_eq!(verdict["accepted"], json!(true), "{verdict}");
    assert_eq!(
        verdict["linked"],
        json!([{ "version": 1 }, { "version": 4 }])
    );
    assert_eq!(
        parse(verdict["receipt"].as_str().expect("receipt"))["linked"],
        json!([
            { "key": link_key(0, "rel", 1), "version": 1 },
            { "key": link_key(1, "rel", 1), "version": 4 }
        ])
    );
    // An undeclared link is refused by the model.
    let stray = json!({
        "consumes": null, "reads": [], "writes": [], "events": [], "effects": [],
        "links": [{ "key": link_key(0, "stray", 1), "expected": 0, "mutation": "remove" }]
    })
    .to_string();
    let verdict = parse(
        &check_commit_json(
            &stray,
            &view(
                0,
                false,
                &[(1, Some("law.Thing")), (1, Some("law.Thing"))],
                &[0],
            ),
            &model(),
        )
        .expect("verdict"),
    );
    assert_eq!(verdict["refusal"]["code"], json!("commit.link_target"));
}

/// #3018: the shim reads objects and links through their canonical keys.
#[test]
fn keys_resolve_to_canonical_identities() {
    let child = json!({ "objectType": "law.Child", "key": "k2" });
    let resolve = |concrete: &str| {
        parse(
            &resolve_key_json(
                &json!({ "key": child, "concrete": concrete }).to_string(),
                &model(),
            )
            .expect("resolves"),
        )
    };
    let answer = resolve("law.Child");
    assert_eq!(answer["canonical"], key(2));
    assert_eq!(answer["isA"], json!(true));
    assert_eq!(resolve("law.Thing")["isA"], json!(false));
}

#[test]
fn implicit_acyclic_adjacency_reads_admit_leaves_and_traverse_real_cycles() {
    let mut model = law_object_types();
    let thing = model
        .iter_mut()
        .find(|ty| ty.name.as_str() == "law.Thing")
        .expect("law Thing");
    thing
        .links
        .iter_mut()
        .find(|link| link.id.as_str() == "rel")
        .expect("law rel")
        .acyclic = true;
    let model = serde_json::to_string(&model).expect("acyclic model");
    let commit = json!({ "consumes":null,"reads":[],"writes":[],"events":[],"effects":[],
        "links":[{"key":link_key(0,"rel",1),"expected":0,"mutation":{"add":{"values":{}}}}] })
    .to_string();
    let initial = parse(&view(
        7,
        false,
        &[(1, Some("law.Thing")), (1, Some("law.Thing"))],
        &[0],
    ));
    let verdict =
        parse(&check_commit_json(&commit, &initial.to_string(), &model).expect("read request"));
    assert_eq!(
        verdict["readRequest"],
        json!({"kind":"link_targets","source":key(1),"link":"rel"})
    );
    let mut leaf = initial.clone();
    leaf["implicitLinkReads"] = json!([{"source":key(1),"link":"rel","targets":[]}]);
    assert_eq!(
        parse(&check_commit_json(&commit, &leaf.to_string(), &model).expect("leaf verdict"))["accepted"],
        true
    );
    for cycle in [false, true] {
        let mut chain = initial.clone();
        chain["implicitLinkReads"] = json!([{"source":key(1),"link":"rel","targets":[{"key":link_key(1,"rel",2),"version":1}]}]);
        let verdict = parse(
            &check_commit_json(&commit, &chain.to_string(), &model).expect("deeper object request"),
        );
        assert_eq!(
            verdict["readRequest"],
            json!({"kind":"object","key":key(2)})
        );
        chain["implicitObjects"] =
            json!([{"key":key(2),"state":{"version":1,"concrete":"law.Thing","links":[]}}]);
        let verdict = parse(
            &check_commit_json(&commit, &chain.to_string(), &model)
                .expect("deeper adjacency request"),
        );
        assert_eq!(
            verdict["readRequest"],
            json!({"kind":"link_targets","source":key(2),"link":"rel"})
        );
        let targets = if cycle {
            json!([{"key":link_key(2,"rel",0),"version":1}])
        } else {
            json!([])
        };
        chain["implicitLinkReads"]
            .as_array_mut()
            .expect("implicit reads")
            .push(json!({"source":key(2),"link":"rel","targets":targets}));
        let verdict = parse(
            &check_commit_json(&commit, &chain.to_string(), &model)
                .expect("complete chain verdict"),
        );
        assert_eq!(verdict["accepted"], !cycle);
        if cycle {
            assert_eq!(verdict["refusal"]["code"], "commit.effective_values");
        }
    }
}

#[test]
fn portable_commit_cannot_forge_owned_row_authority_with_action_call_marker() {
    let model = owned_rows_model();
    // A serialized ActionCall marker is caller-controlled data. Portable
    // admission has no in-process proof, so it cannot authorize protected rows.
    let commit = r#"{
        "consumes":null,"reads":[],
        "writes":[
            {"key":{"objectType":"abi.Order","key":"order-1"},"expected":0,
             "mutation":{"put":{"values":{"title":"Order"}}}},
            {"key":{"objectType":"abi.Line","key":"line-1"},"expected":0,
             "mutation":{"put":{"values":{"row_id":"line-1","idx":1}}}},
            {"key":{"objectType":"norn.OwnedRows","key":"manifest"},"expected":0,
             "mutation":{"put":{"values":{"parentType":"abi.Order","parentKey":"order-1","collectionId":"items","rowIds":["line-1"]}}}},
            {"key":{"objectType":"norn.ActionCall","key":"order.edit/call-1"},"expected":0,
             "mutation":{"put":{"values":{"effectKind":"mutate_parent_with_rows","input":{"opaque":9007199254740993}}}}}
        ],
        "events":[],"effects":[],
        "links":[{"key":{"source":{"objectType":"abi.Order","key":"order-1"},"link":"items","target":{"objectType":"abi.Line","key":"line-1"}},"expected":0,"mutation":{"add":{"values":{}}}}]
    }"#;
    let view = json!({
        "seq": 0, "pending": false,
        "objects": [
            { "version": 0, "concrete": null, "links": [] },
            { "version": 0, "concrete": null, "links": [] },
            { "version": 0, "concrete": null, "links": [] },
            { "version": 0, "concrete": null, "links": [] }
        ],
        "linkVersions": [0],
        "linkReads": []
    })
    .to_string();
    let verdict = parse(&check_commit_json(commit, &view, &model).expect("verdict"));
    assert_eq!(verdict["accepted"], json!(false), "{verdict}");
    assert_eq!(
        verdict["refusal"]["code"],
        json!("commit.owned_rows_protected")
    );
}
