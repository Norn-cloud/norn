#![allow(
    clippy::unwrap_used,
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

//! T12-DP-1 acceptance: `cargo test -p norn-dataplane raw_observation`.
//!
//! Contract/schema fixtures cover a mutation, a delete, a cursor, and
//! deterministic redaction provenance; the Iceberg spec is checked against the
//! contract; the convergence view is checked for duplicate and out-of-order
//! delivery.

use std::collections::BTreeSet;

use norn_dataplane::iceberg::{
    RAW_OBSERVATION_COLUMNS, RAW_OBSERVATION_FORMAT_VERSION, RAW_OBSERVATION_TABLE,
    create_table_request, partition_spec, sort_order, table_schema,
};
use norn_dataplane::{
    Applied, Convergence, ObservationDraft, ObservationError, ObservationKind,
    RAW_OBSERVATION_CONTRACT, REDACTED, RawObservation, RedactionPolicy, parse_exact_json,
    raw_observation_schema,
};
use serde::Deserialize;
use serde_json::{Value, json};

struct Fixture {
    #[allow(dead_code, reason = "documents the fixture")]
    description: String,
    draft: ObservationDraft,
    policy: Option<RedactionPolicy>,
    expect: Option<Expect>,
}

#[derive(Deserialize)]
struct FixtureWire {
    description: String,
    draft: Value,
    policy: Option<RedactionPolicy>,
    #[serde(default)]
    expect: Option<Expect>,
}

#[derive(Deserialize)]
struct Expect {
    redacted: Vec<String>,
    payload: Value,
}

fn fixture(name: &str) -> Fixture {
    let path = format!(
        "{}/tests/fixtures/raw_observation/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let wire: FixtureWire = serde_json::from_value(parse_exact_json(text.as_bytes()).unwrap())
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    Fixture {
        description: wire.description,
        draft: ObservationDraft::from_json_slice(
            serde_json::to_string(&wire.draft).unwrap().as_bytes(),
        )
        .unwrap(),
        policy: wire.policy,
        expect: wire.expect,
    }
}

fn sealed(name: &str) -> RawObservation {
    let fixture = fixture(name);
    fixture
        .draft
        .seal(fixture.policy.as_ref())
        .unwrap_or_else(|error| panic!("{name}: {error}"))
}

const FIXTURES: &[&str] = &["mutation", "delete", "cursor", "redaction"];

#[test]
fn raw_observation_fixtures_seal_validate_and_round_trip() {
    for name in FIXTURES {
        let observation = sealed(name);
        observation.validate().unwrap();
        assert_eq!(observation.contract, RAW_OBSERVATION_CONTRACT);
        let encoded = serde_json::to_string(&observation).unwrap();
        let decoded = RawObservation::from_json_slice(encoded.as_bytes()).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, observation, "{name} round-trips");
        assert_eq!(
            observation.observation_id(),
            format!(
                "{}/{}/{}",
                observation.source, observation.source_id, observation.event_id
            )
        );
    }
}

#[test]
fn raw_observation_mutation_and_delete_are_both_observations() {
    let mutation = sealed("mutation");
    let delete = sealed("delete");
    assert_eq!(mutation.kind, ObservationKind::Upsert);
    assert_eq!(delete.kind, ObservationKind::Delete);
    assert_eq!(mutation.source_id, delete.source_id);
    assert_ne!(mutation.event_id, delete.event_id);
    // The delete carries the tombstone as received; nothing is removed.
    assert_eq!(delete.payload["docstatus"], json!(2));
    assert!(mutation.cursor.is_none());
    assert!(mutation.redaction.is_none());
}

#[test]
fn raw_observation_cursor_is_recorded_verbatim() {
    let observation = sealed("cursor");
    assert_eq!(
        observation.cursor,
        Some(json!({
            "doctype": "Sales Order",
            "modified": "2026-10-01 09:30:00.100000",
            "name": "SO-0001"
        }))
    );
    assert_eq!(
        observation.pipeline_version,
        "norn-dataplane.frappe-sweep/1"
    );
}

#[test]
fn raw_observation_redaction_is_deterministic_and_recorded() {
    let fixture = fixture("redaction");
    let expect = fixture.expect.as_ref().unwrap();
    let policy = fixture.policy.clone().unwrap();
    let first = fixture.draft.clone().seal(Some(&policy)).unwrap();
    let second = fixture.draft.clone().seal(Some(&policy)).unwrap();
    assert_eq!(first, second, "same policy, same input, same observation");
    assert_eq!(first.payload, expect.payload);
    let record = first.redaction.clone().unwrap();
    assert_eq!(record.policy_id, "pii.customer");
    assert_eq!(record.policy_version, "2026-10-01");
    assert_eq!(record.redacted, expect.redacted);
    assert!(!first.payload.to_string().contains("example.com"));

    // Idempotent: re-applying the policy changes nothing further.
    let mut again = first.payload.clone();
    let record_again = policy.apply(&mut again).unwrap();
    assert_eq!(again, first.payload);
    assert_eq!(record_again, record);

    // A new policy version is visible in provenance and the digest is of the
    // redacted payload.
    let unredacted = fixture.draft.clone().seal(None).unwrap();
    assert_ne!(unredacted.payload_digest, first.payload_digest);
    assert_eq!(first.payload["mobile_no"], json!(REDACTED));
}

#[test]
fn raw_observation_redaction_rejects_invalid_pointers() {
    let mut payload = json!({"a": 1});
    for pointer in ["", "a", "a/b"] {
        let policy = RedactionPolicy {
            id: "p".to_owned(),
            version: "1".to_owned(),
            pointers: vec![pointer.to_owned()],
        };
        assert!(matches!(
            policy.apply(&mut payload),
            Err(ObservationError::InvalidPointer { .. })
        ));
    }
    let unversioned = RedactionPolicy {
        id: "p".to_owned(),
        version: String::new(),
        pointers: vec!["/a".to_owned()],
    };
    assert!(matches!(
        unversioned.apply(&mut payload),
        Err(ObservationError::Empty { .. })
    ));
    assert_eq!(payload, json!({"a": 1}));
}

#[test]
fn raw_observation_validate_refuses_tampering() {
    let good = sealed("mutation");

    let mut tampered = good.clone();
    tampered.payload["grand_total"] = json!(1);
    assert!(matches!(
        tampered.validate(),
        Err(ObservationError::DigestMismatch { .. })
    ));

    let mut wrong_contract = good.clone();
    wrong_contract.contract = "norn.raw_observation/v0".to_owned();
    assert!(matches!(
        wrong_contract.validate(),
        Err(ObservationError::ContractMismatch { .. })
    ));

    let mut slashed = good.clone();
    slashed.source = "erp/next".to_owned();
    assert!(matches!(
        slashed.validate(),
        Err(ObservationError::InvalidSource { .. })
    ));

    let mut empty_event = good.clone();
    empty_event.event_id = " ".to_owned();
    assert!(matches!(
        empty_event.validate(),
        Err(ObservationError::Empty { field: "event_id" })
    ));

    let mut value = serde_json::to_value(&good).unwrap();
    value["extra"] = json!(true);
    assert!(
        RawObservation::from_json_slice(serde_json::to_string(&value).unwrap().as_bytes()).is_err()
    );
}

#[test]
fn raw_observation_json_schema_matches_the_contract() {
    let schema = serde_json::to_value(raw_observation_schema()).unwrap();
    let properties: BTreeSet<String> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let columns: BTreeSet<String> = RAW_OBSERVATION_COLUMNS
        .iter()
        .map(|column| column.name.to_owned())
        .collect();
    assert_eq!(properties, columns);
    let required: BTreeSet<String> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    let required_columns: BTreeSet<String> = RAW_OBSERVATION_COLUMNS
        .iter()
        .filter(|column| column.required)
        .map(|column| column.name.to_owned())
        .collect();
    assert_eq!(required, required_columns);
    assert_eq!(schema["additionalProperties"], json!(false));
}

#[test]
fn raw_observation_iceberg_spec_matches_the_contract() {
    // Every contract field is a column with the same name, and vice versa.
    let full = sealed("redaction");
    let mut with_cursor = full;
    with_cursor.cursor = Some(json!({"page": 1}));
    let encoded = serde_json::to_value(&with_cursor).unwrap();
    let fields: BTreeSet<String> = encoded.as_object().unwrap().keys().cloned().collect();
    let columns: BTreeSet<String> = RAW_OBSERVATION_COLUMNS
        .iter()
        .map(|column| column.name.to_owned())
        .collect();
    assert_eq!(fields, columns);

    // The Iceberg schema lists exactly those columns with the same ids.
    let schema = table_schema();
    let iceberg: Vec<(u64, String, bool)> = schema["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| {
            (
                field["id"].as_u64().unwrap(),
                field["name"].as_str().unwrap().to_owned(),
                field["required"].as_bool().unwrap(),
            )
        })
        .collect();
    let expected: Vec<(u64, String, bool)> = RAW_OBSERVATION_COLUMNS
        .iter()
        .map(|column| {
            (
                u64::from(column.id),
                column.name.to_owned(),
                column.required,
            )
        })
        .collect();
    assert_eq!(iceberg, expected);
    assert_eq!(schema["fields"][6]["type"], json!("variant"));
    assert_eq!(schema["fields"][8]["type"], json!("timestamptz"));

    // Field ids are unique across nesting levels.
    let mut ids = Vec::new();
    collect_ids(&schema, &mut ids);
    let unique: BTreeSet<u64> = ids.iter().copied().collect();
    assert_eq!(unique.len(), ids.len(), "duplicate Iceberg field id");

    // Partition and sort fields reference real columns.
    let known: BTreeSet<u64> = ids.iter().copied().collect();
    for field in partition_spec()["fields"].as_array().unwrap() {
        assert!(known.contains(&field["source-id"].as_u64().unwrap()));
    }
    for field in sort_order()["fields"].as_array().unwrap() {
        assert!(known.contains(&field["source-id"].as_u64().unwrap()));
    }

    // Append-only: no identifier fields (no row-level upsert key).
    assert!(schema.get("identifier-field-ids").is_none());

    let request = create_table_request();
    assert_eq!(request["name"], json!(RAW_OBSERVATION_TABLE));
    assert_eq!(
        request["properties"]["format-version"],
        json!(RAW_OBSERVATION_FORMAT_VERSION.to_string())
    );
    assert_eq!(request["properties"]["norn.append-only"], json!("true"));
    assert_eq!(
        request["properties"]["norn.contract"],
        json!(RAW_OBSERVATION_CONTRACT)
    );
}

fn collect_ids(value: &Value, ids: &mut Vec<u64>) {
    match value {
        Value::Object(map) => {
            for key in ["id", "element-id", "key-id", "value-id"] {
                if let Some(id) = map.get(key).and_then(Value::as_u64) {
                    ids.push(id);
                }
            }
            for child in map.values() {
                collect_ids(child, ids);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_ids(item, ids);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[test]
fn raw_observation_duplicates_and_reordering_converge() {
    let mutation = sealed("mutation");
    let swept = sealed("cursor");
    let delete = sealed("delete");

    let orders: [[&RawObservation; 5]; 4] = [
        [&mutation, &swept, &delete, &mutation, &delete],
        [&delete, &swept, &mutation, &swept, &mutation],
        [&swept, &delete, &delete, &mutation, &swept],
        [&mutation, &mutation, &delete, &swept, &swept],
    ];
    let reference = Convergence::from_log([&mutation, &swept, &delete]);
    let current = reference.current("erpnext", "Sales Order/SO-0001").unwrap();
    assert_eq!(current.kind, ObservationKind::Delete);
    assert_eq!(current.event_id, delete.event_id);
    for order in orders {
        assert_eq!(Convergence::from_log(order), reference);
    }

    let mut view = Convergence::new();
    assert_eq!(view.apply(&swept), Applied::Advanced);
    assert_eq!(view.apply(&mutation), Applied::Stale, "older version");
    assert_eq!(view.apply(&mutation), Applied::Duplicate);
    assert_eq!(view.apply(&delete), Applied::Advanced);
    assert!(view.has_seen("erpnext", &mutation.source_id, &mutation.event_id));

    // A redelivery with a different receipt time is still the same event.
    let mut redelivered = mutation.clone();
    redelivered.extracted_at = redelivered.extracted_at.saturating_add(60_000);
    assert_eq!(view.apply(&redelivered), Applied::Duplicate);
    assert_eq!(view, reference);
}

#[test]
fn raw_observation_submission_matches_the_observe_route() {
    let observation = sealed("cursor");
    let submission = observation.submission().unwrap();
    assert_eq!(submission.source, "erpnext");
    let body = serde_json::to_value(&submission).unwrap();
    let keys: BTreeSet<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(["sourceId", "eventId", "mappingVersion", "payload"])
    );
    assert_eq!(body["sourceId"], json!("Sales Order/SO-0001"));
    assert_eq!(
        body["mappingVersion"],
        json!("norn-dataplane.frappe-sweep/1")
    );
    let carried = RawObservation::from_json_slice(
        serde_json::to_string(&body["payload"]).unwrap().as_bytes(),
    )
    .unwrap();
    carried.validate().unwrap();
    assert_eq!(carried, observation);
    assert_eq!(
        format!(
            "{}/{}/{}",
            submission.source, submission.source_id, submission.event_id
        ),
        observation.observation_id()
    );
}

#[test]
fn raw_observation_shared_event_ids_do_not_suppress_other_records() {
    // Two records whose source reused one event id: both are kept, in
    // either delivery order (the dedup key is the full observation id).
    let first = sealed("mutation");
    let mut other = sealed("redaction");
    other.event_id = first.event_id.clone();
    for order in [[&first, &other], [&other, &first]] {
        let view = Convergence::from_log(order);
        assert!(view.current("erpnext", &first.source_id).is_some());
        assert!(view.current("erpnext", &other.source_id).is_some());
        assert!(view.has_seen("erpnext", &other.source_id, &other.event_id));
    }
}

#[test]
fn raw_observation_payloads_are_parsed_exactly() {
    // Within f64 precision, and different spellings of one number: accepted.
    let value = parse_exact_json(br#"{"a": 1.50, "b": 1e2, "c": 0.1, "d": [1, -0]}"#).unwrap();
    assert_eq!(value["a"], json!(1.5));
    // Beyond f64 precision: refused, never silently rounded.
    assert!(matches!(
        parse_exact_json(br#"{"a": 1.00000000000000001}"#),
        Err(ObservationError::LossyPayload)
    ));
    // Duplicate keys: refused, never silently collapsed.
    assert!(parse_exact_json(br#"{"a": 1, "a": 2}"#).is_err());
    assert!(parse_exact_json(b"not json").is_err());

    // Stored observations decode through the same exact path.
    let observation = sealed("mutation");
    let bytes = serde_json::to_vec(&observation).unwrap();
    assert_eq!(
        RawObservation::from_json_slice(&bytes).unwrap(),
        observation
    );
    let mut tampered = serde_json::to_value(&observation).unwrap();
    tampered["payload"]["grand_total"] = json!(7);
    let tampered = serde_json::to_vec(&tampered).unwrap();
    assert!(matches!(
        RawObservation::from_json_slice(&tampered),
        Err(ObservationError::DigestMismatch { .. })
    ));
}

#[test]
fn raw_observation_overlapping_redaction_is_idempotent() {
    let policy = RedactionPolicy {
        id: "p".to_owned(),
        version: "1".to_owned(),
        pointers: vec!["/*/secret".to_owned(), "/a".to_owned()],
    };
    let mut payload = json!({"a": {"secret": 1, "x": 2}, "b": {"secret": 3}});
    let first = policy.apply(&mut payload).unwrap();
    assert_eq!(first.redacted, ["/a", "/b/secret"]);
    assert_eq!(payload, json!({"a": REDACTED, "b": {"secret": REDACTED}}));
    let mut again = payload.clone();
    let second = policy.apply(&mut again).unwrap();
    assert_eq!(second, first);
    assert_eq!(again, payload);
}

#[test]
fn raw_observation_public_decoders_refuse_evidence_loss() {
    let draft = fixture("mutation").draft;
    let mut value = serde_json::to_value(&draft).unwrap();
    value["payload"] = json!({"exact": 1});
    let draft_json = serde_json::to_string(&value).unwrap();
    let sealed = ObservationDraft::from_json_slice(draft_json.as_bytes())
        .unwrap()
        .seal(None)
        .unwrap();
    let raw_json = serde_json::to_string(&sealed).unwrap();
    for document in [&draft_json, &raw_json] {
        for altered in [
            document.replace("\"exact\":1", "\"exact\":1.00000000000000001"),
            document.replace("\"exact\":1", "\"exact\":1.00000000000000002"),
            document.replace("\"exact\":1", "\"exact\":1,\"exact\":2"),
        ] {
            assert_ne!(&altered, document);
            assert!(ObservationDraft::from_json_slice(altered.as_bytes()).is_err());
            assert!(RawObservation::from_json_slice(altered.as_bytes()).is_err());
        }
    }
}
