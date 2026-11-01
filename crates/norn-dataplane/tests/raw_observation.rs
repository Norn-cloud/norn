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
//! deterministic redaction provenance and the JSON schema/serialized field contract.
//! Iceberg-specific correspondence belongs to the separately parked table layer.

use norn_dataplane::{
    ObservationDraft, ObservationError, ObservationKind, RAW_OBSERVATION_CONTRACT, REDACTED,
    RawObservation, RedactionPolicy, parse_exact_json, raw_observation_schema,
};
use proptest::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

struct Fixture {
    expected_id: String,
    draft: ObservationDraft,
    policy: Option<RedactionPolicy>,
    expect: Option<Expect>,
}

#[derive(Deserialize)]
struct FixtureWire {
    description: String,
    expect_observation_id: String,
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
    assert!(!wire.description.trim().is_empty());
    Fixture {
        expected_id: wire.expect_observation_id,
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
        assert_eq!(observation.observation_id(), fixture(name).expected_id);
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
    let instance = serde_json::to_value(sealed("mutation")).unwrap();
    let columns: BTreeSet<String> = instance.as_object().unwrap().keys().cloned().collect();
    assert_eq!(properties, columns);
    let required: BTreeSet<String> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    let required_columns: BTreeSet<String> = [
        "contract",
        "source",
        "source_id",
        "event_id",
        "kind",
        "source_version",
        "payload",
        "payload_digest",
        "extracted_at",
        "pipeline_version",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(required, required_columns);
    assert_eq!(schema["additionalProperties"], json!(false));
}

proptest! {
    #[test]
    fn raw_observation_identity_components_do_not_alias(a in "[a-z%/]{1,12}", b in "[a-z%/]{1,12}", c in "[a-z%/]{1,12}") {
        let mut first = sealed("mutation");
        first.source_id = format!("{a}/{b}");
        first.event_id = c.clone();
        let mut second = first.clone();
        second.source_id = a;
        second.event_id = format!("{b}/{c}");
        prop_assert_ne!(first.observation_id(), second.observation_id());
    }
}
