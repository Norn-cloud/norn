#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use norn_ir::{
    ConstraintClass, DataQualityFinding, DataQualityFindingStatus, DataQualityLineage,
    DataQualityOutcome, DataQualityRuleRef, DataQualitySeverity, DataQualitySubject,
    InstanceProvenance, Invariant, InvariantId, InvariantOwner, MappingVersion, ObjectTypeName,
    PropertyId, SourceRecordRef, UtcTimestamp, mapping::SnapshotDeviationReason,
};

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp(value.to_owned())
}

fn invariant_rule(version: &str) -> DataQualityRuleRef {
    DataQualityRuleRef::Invariant {
        owner: InvariantOwner::Object(ObjectTypeName("crm.Customer".to_owned())),
        invariant_id: InvariantId("valid_email".to_owned()),
        version: version.to_owned(),
    }
}

fn lineage(recorded_at: &str, observation_ids: &[&str]) -> DataQualityLineage {
    DataQualityLineage::new(
        InstanceProvenance {
            source: SourceRecordRef::new("CRM", "Customer/C-100"),
            mapping_version: MappingVersion("crm.customer@3".to_owned()),
            transform: Some("customer.email".to_owned()),
            observed_at: Some(timestamp("2026-10-01T09:30:00Z")),
            recorded_at: timestamp(recorded_at),
            source_fields: Vec::new(),
            markings: Vec::new(),
            upstream: Vec::new(),
        },
        observation_ids.iter().map(|id| (*id).to_owned()).collect(),
    )
}

fn finding(
    observation_id: &str,
    first_seen: &str,
    last_seen: &str,
    recorded_at: &str,
    status: DataQualityFindingStatus,
    severity: DataQualitySeverity,
) -> DataQualityFinding {
    DataQualityFinding::v2(
        invariant_rule("0.5.0"),
        vec![DataQualitySubject::new(
            ObjectTypeName("crm.Customer".to_owned()),
            "C-100",
        )],
        severity,
        "email must be valid",
        DataQualityOutcome::NotSatisfied,
        vec![lineage(recorded_at, &[observation_id])],
        timestamp(first_seen),
        timestamp(last_seen),
        status,
    )
    .expect("finding identity should canonicalize")
}

#[test]
fn omitted_invariant_classification_defaults_to_integrity_without_changing_wire_shape() {
    let invariant: Invariant =
        serde_json::from_str(r#"{"id":"valid_email","expression":"true","message":"valid"}"#)
            .expect("legacy invariant decodes");
    assert_eq!(invariant.classification, ConstraintClass::Integrity);
    assert_eq!(
        serde_json::to_value(&invariant).expect("serialize invariant"),
        serde_json::json!({
            "id": "valid_email",
            "expression": "true",
            "message": "valid"
        })
    );

    let quality: Invariant = serde_json::from_value(serde_json::json!({
        "id": "valid_email",
        "classification": "quality",
        "expression": "true",
        "message": "valid"
    }))
    .expect("quality invariant decodes");
    assert_eq!(quality.classification, ConstraintClass::Quality);
    assert_eq!(
        serde_json::to_value(quality).expect("serialize quality invariant")["classification"],
        "quality"
    );
}

#[test]
fn finding_identity_is_stable_across_replays_and_tracks_changed_evidence() {
    let first = finding(
        "observation-1",
        "2026-10-01T09:30:00Z",
        "2026-10-01T09:30:00Z",
        "2026-10-01T09:30:01Z",
        DataQualityFindingStatus::Open,
        DataQualitySeverity::Warning,
    );
    let replay = finding(
        "observation-2",
        "2026-10-01T09:30:00Z",
        "2026-10-02T12:00:00Z",
        "2026-10-02T12:00:01Z",
        DataQualityFindingStatus::Acknowledged,
        DataQualitySeverity::Error,
    );

    assert_eq!(first.id, replay.id);
    assert_eq!(first.dedupe_key, replay.dedupe_key);
    assert_ne!(first.provenance, replay.provenance);
    assert_eq!(first.first_seen, replay.first_seen);
    assert_ne!(first.last_seen, replay.last_seen);

    let changed_evidence = first
        .clone()
        .with_evidence_digest("sha256:changed-value")
        .expect("evidence digest should canonicalize");
    assert_ne!(first.id, changed_evidence.id);
    assert_ne!(first.dedupe_key, changed_evidence.dedupe_key);

    let changed_rule_version = DataQualityFinding::v2(
        invariant_rule("0.6.0"),
        first.subjects.clone(),
        first.severity,
        first.message.clone(),
        first.outcome.clone(),
        first.provenance.clone(),
        first.first_seen.clone(),
        first.last_seen.clone(),
        first.status,
    )
    .expect("finding identity should canonicalize");
    assert_ne!(first.id, changed_rule_version.id);
}

#[test]
fn finding_wire_shape_is_versioned_and_keeps_observation_lineage() {
    let finding = finding(
        "observation-1",
        "2026-10-01T09:30:00Z",
        "2026-10-01T09:30:00Z",
        "2026-10-01T09:30:01Z",
        DataQualityFindingStatus::Open,
        DataQualitySeverity::Warning,
    );
    let json = serde_json::to_value(&finding).expect("finding serializes");
    assert_eq!(json["schemaVersion"], 2);
    assert_eq!(json["id"], finding.id);
    assert_eq!(json["dedupeKey"], finding.dedupe_key);
    assert_eq!(json["ruleRef"]["invariantId"], "valid_email");
    assert!(json["ruleRef"].get("invariant_id").is_none());
    assert_eq!(json["status"], "open");
    assert_eq!(json["severity"], "warning");
    assert_eq!(json["provenance"][0]["observationIds"][0], "observation-1");
    assert!(json.get("evidenceDigest").is_none());

    let round_trip: DataQualityFinding = serde_json::from_value(json).expect("finding decodes");
    assert_eq!(finding, round_trip);
}

#[test]
fn unified_outcomes_keep_each_producer_kind_and_its_context() {
    let out_of_band = serde_json::to_value(DataQualityOutcome::ObservedOutOfBand {
        property: PropertyId("status".to_owned()),
        from: Some("draft".to_owned()),
        state: "approved".to_owned(),
        reason: SnapshotDeviationReason::NoDeclaredPath,
    })
    .expect("out-of-band outcome serializes");
    assert_eq!(out_of_band["kind"], "observed_out_of_band");
    assert_eq!(out_of_band["reason"], "no_declared_path");

    let invalid_temporal = serde_json::to_value(DataQualityOutcome::InvalidTemporalObservation {
        property: PropertyId("issued_at".to_owned()),
        value: "2026-02-30T12:00:00Z".to_owned(),
        reason: "invalid_calendar_date".to_owned(),
    })
    .expect("temporal outcome serializes");
    assert_eq!(invalid_temporal["kind"], "invalid_temporal_observation");
    assert_eq!(invalid_temporal["value"], "2026-02-30T12:00:00Z");

    let identity = serde_json::to_value(DataQualityOutcome::IdentityProjectionIsolation {
        code: "identity.row.invalid".to_owned(),
        detail: "the record is outside the configured cell scope".to_owned(),
    })
    .expect("identity outcome serializes");
    assert_eq!(identity["kind"], "identity_projection_isolation");
    assert_eq!(identity["code"], "identity.row.invalid");
}
