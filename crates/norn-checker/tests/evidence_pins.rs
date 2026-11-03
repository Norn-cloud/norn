//! PROOF-BIND-1 / PROOF-STALE identity correspondence; no solver is simulated.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "evidence identity scenarios"
)]
use norn_checker::obligations::evidence::{
    EvidenceError, EvidenceIdentity, EvidencePins, EvidenceRecord, EvidenceTool, STALE_EVIDENCE,
};
use norn_checker::obligations::{ApprovedContract, Challenge, DefinitionSource};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn pool() -> Vec<Value> {
    let mut artifacts: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json"))
            .expect("generated entities");
    artifacts.extend(
        serde_json::from_str::<Vec<Value>>(include_str!("../../../fixtures/law-ir/laws.json"))
            .expect("generated LawIR"),
    );
    let property = norn_ir::PropertyType::Decimal {
        scale: norn_ir::WholeNumber(3),
        rounding: None,
        unit: Some(norn_ir::UnitRef {
            system: norn_ir::UnitSystem::Ucum,
            code: "g".into(),
        }),
        currency_source: None,
        per_unit: None,
    };
    let mut mass = artifacts[0]["payload"]["objectType"]["properties"][1].clone();
    mass["id"] = json!("mass");
    mass["type"] = serde_json::to_value(property).expect("Rust-authored quantity type");
    artifacts[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(mass);
    let pin = norn_ir::ArtifactDependency {
        artifact_id: "core.units".into(),
        kind: norn_ir::ArtifactKind::Entity,
        version: Some("1.0.0".into()),
        required: true,
    };
    artifacts[0]["dependencies"] = serde_json::to_value(vec![pin]).expect("unit dependency");
    artifacts.push(
        serde_json::from_str(include_str!(
            "../../../registry/core/artifacts/core.units.json"
        ))
        .expect("retained unit catalog"),
    );
    artifacts
}

fn challenge(artifacts: &[Value]) -> Challenge {
    let typed: Vec<norn_ir::ArtifactEnvelope> = artifacts
        .iter()
        .map(|a| serde_json::from_value(a.clone()).expect("typed artifact"))
        .collect();
    let findings = norn_checker::check_artifact_set(
        &typed,
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(findings.is_empty(), "fixture admission: {findings:?}");
    let source = DefinitionSource::parse(
        &serde_json::to_string(artifacts).expect("exact source"),
        "[]",
    )
    .expect("parse admitted source");
    let registry = BTreeSet::new();
    ApprovedContract::approve(
        &source,
        &registry,
        json!({"expression":"norn-zen/6","units":"pinned"}),
        json!({"host":"single-cell"}),
        json!({"required":"bounded","checker":"unknown"}),
    )
    .expect("admission")
    .challenge(&source, &registry)
    .expect("checker-generated goals")
}
fn tool() -> EvidenceTool {
    EvidenceTool {
        id: "test.identity-only".into(),
        version: "1".into(),
        implementation_sha256: "a".repeat(64),
    }
}
fn pins(challenge: &Challenge, seed: u64) -> EvidencePins {
    challenge
        .evidence_pins(0, tool(), r#"{"steps":8,"fairness":[]}"#, seed)
        .expect("generated goal identity")
}
fn stale(challenge: &Challenge, current: &EvidencePins, digest: &str, record: &EvidenceRecord) {
    let identity = challenge.evidence_identity(current, digest, record);
    assert_eq!(identity, EvidenceIdentity::Unknown);
    assert_eq!(identity.code(), Some(STALE_EVIDENCE));
}

#[test]
fn matching_pass_and_counterexample_payloads_establish_identity_only() {
    let challenge = challenge(&pool());
    let pins = pins(&challenge, 0);
    for payload in [
        r#"{"status":"bounded-pass","reachable":true}"#,
        r#"{"status":"counterexample","trace":[100,99.2]}"#,
        r#"{"status":"proved"}"#,
    ] {
        let record = challenge
            .record_evidence(&pins, payload)
            .expect("opaque claim");
        assert_eq!(record.result_json, payload);
        assert_eq!(
            serde_json::from_str::<Value>(&record.challenge_json).expect("complete challenge"),
            *challenge.body()
        );
        assert_eq!(
            challenge.evidence_identity(&pins, &record.result_sha256, &record),
            EvidenceIdentity::Current
        );
        let decoded: EvidenceRecord =
            serde_json::from_str(&serde_json::to_string(&record).expect("wire"))
                .expect("round trip");
        assert_eq!(decoded, record);
    }
    assert_eq!(EvidenceIdentity::Current.code(), None);
}

#[test]
fn exact_payload_bytes_and_controller_result_digest_cannot_be_substituted() {
    let challenge = challenge(&pool());
    let pins = pins(&challenge, 42);
    let original = challenge
        .record_evidence(&pins, r#"{"value":1,"label":"é"}"#)
        .expect("result");
    for payload in [
        "{ \"value\":1,\"label\":\"é\"}",
        r#"{"value":1.0,"label":"é"}"#,
        "{\"value\":1,\"label\":\"e\u{301}\"}",
    ] {
        let replacement = challenge
            .record_evidence(&pins, payload)
            .expect("distinct exact bytes");
        assert_ne!(replacement.result_sha256, original.result_sha256);
        stale(&challenge, &pins, &original.result_sha256, &replacement);
    }
}

#[test]
fn malformed_profiles_and_absent_generated_goals_refuse() {
    let challenge = challenge(&pool());
    for bounds in ["{}", "[]", "null", "{", r#"{"steps":1,"steps":2}"#] {
        assert!(challenge.evidence_pins(0, tool(), bounds, 0).is_err());
    }
    assert_eq!(
        challenge.evidence_pins(u32::MAX, tool(), r#"{"steps":1}"#, 0),
        Err(EvidenceError::MissingGoal)
    );
    for bad in [
        EvidenceTool {
            id: " ".into(),
            ..tool()
        },
        EvidenceTool {
            version: String::new(),
            ..tool()
        },
        EvidenceTool {
            implementation_sha256: "A".repeat(64),
            ..tool()
        },
        EvidenceTool {
            id: "a".repeat(1_025),
            ..tool()
        },
    ] {
        assert_eq!(
            challenge.evidence_pins(0, bad, r#"{"steps":1}"#, 0),
            Err(EvidenceError::Malformed)
        );
    }
    assert_eq!(
        challenge.evidence_pins(0, tool(), &" ".repeat(65_537), 0),
        Err(EvidenceError::Limit)
    );
    let pins = pins(&challenge, 0);
    assert_eq!(
        challenge.record_evidence(&pins, &" ".repeat(1_048_577)),
        Err(EvidenceError::Limit)
    );
    assert_eq!(
        challenge.record_evidence(&pins, "{"),
        Err(EvidenceError::Malformed)
    );
    let mut record = serde_json::to_value(
        challenge
            .record_evidence(&pins, "null")
            .expect("opaque JSON"),
    )
    .expect("wire");
    record["verified"] = json!(true);
    assert!(serde_json::from_value::<EvidenceRecord>(record).is_err());
}

fn mapping_pool() -> Vec<Value> {
    let mut pool = pool();
    let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("standard generated fixtures");
    let core: Vec<Value> = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(&root).join("core-artifacts.json"))
            .expect("typed compiler output"),
    )
    .expect("artifacts");
    let mut source = core
        .iter()
        .find(|a| a["artifactId"] == "toy.contacts.source")
        .expect("generated schema")
        .clone();
    source["artifactId"] = json!("toy.schema");
    source["payload"]["fields"][0]["name"] = json!("id");
    let typed: norn_ir::ArtifactEnvelope =
        serde_json::from_value(source.clone()).expect("typed schema");
    let digest = norn_checker::ArtifactRef::of(&typed)
        .expect("actual source digest")
        .sha256;
    for mut artifact in core
        .into_iter()
        .filter(|a| a["artifactId"] != "toy.contacts.source")
    {
        if artifact["artifactId"] == "toy.Row.entity" {
            artifact["payload"]["objectType"]["implements"] =
                serde_json::to_value(vec![norn_ir::InterfaceName("toy.Identified".into())])
                    .expect("Rust-authored implementation");
            artifact["dependencies"] = serde_json::to_value(vec![norn_ir::ArtifactDependency {
                artifact_id: "toy.Identified.interface".into(),
                kind: norn_ir::ArtifactKind::Entity,
                version: Some("1.0.0".into()),
                required: true,
            }])
            .expect("exact interface dependency");
        }
        if artifact["artifactId"] == "toy.rows" {
            artifact["payload"]["source"]["sha256"] = json!(digest);
            artifact["payload"]["maps"][0]["properties"] =
                serde_json::to_value(vec![norn_ir::mapping::PropertyMap {
                    property: norn_ir::PropertyId("id".into()),
                    value: norn_ir::ZenExpression("input.id".into()),
                    nested: None,
                }])
                .expect("Rust-authored property map");
        }
        pool.push(artifact);
    }
    pool.push(source);
    pool
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn any_pin_or_result_edit_is_unknown(seed in any::<u64>(), field in 0u8..11) {
        let challenge = challenge(&pool());
        let current = pins(&challenge, seed);
        let mut record = challenge.record_evidence(&current, r#"{"status":"bounded-pass"}"#).expect("identity");
        let digest = record.result_sha256.clone();
        match field {
            0 => record.pins.seed ^= 1,
            1 => record.pins.tool.id.push('x'),
            2 => record.pins.tool.version.push('x'),
            3 => record.pins.tool.implementation_sha256.replace_range(..1, "b"),
            4 => record.pins.bounds_json.push(' '),
            5 => record.pins.goal_index = u32::MAX,
            6 => record.pins.goal_sha256.push('0'),
            7 => record.pins.challenge_sha256.push('0'),
            8 => record.challenge_json.push(' '),
            9 => record.result_sha256.push('0'),
            _ => record = challenge.record_evidence(&current, "null").expect("joint payload/hash edit"),
        }
        prop_assert_eq!(challenge.evidence_identity(&current, &digest, &record), EvidenceIdentity::Unknown);
        if (5..=7).contains(&field) {
            prop_assert!(challenge.record_evidence(&record.pins, "null").is_err());
        }
    }

    #[test]
    fn writer_unit_and_mapping_edits_stale_both_result_kinds(kind in 0u8..3, seed in any::<u64>(), counterexample in any::<bool>()) {
        let mut pool = mapping_pool();
        let old = challenge(&pool);
        let old_pins = pins(&old, seed);
        let record = old.record_evidence(&old_pins, if counterexample {r#"{"status":"counterexample"}"#} else {r#"{"status":"bounded-pass"}"#}).expect("result identity");
        match kind {
            0 => pool[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"] = json!(["self.paid == true"]),
            1 => pool[0]["payload"]["objectType"]["properties"][2]["type"]["unit"]["code"] = json!("kg"),
            _ => pool.iter_mut().find(|a| a["artifactId"] == "toy.rows").expect("mapping")["payload"]["maps"][0]["id"] = json!("changed_row"),
        }
        let new = challenge(&pool); // separate approval of the changed reference contract
        let current = pins(&new, seed);
        prop_assert_ne!(old.sha256(), new.sha256());
        prop_assert_eq!(new.evidence_identity(&current, &record.result_sha256, &record), EvidenceIdentity::Unknown);
        prop_assert_eq!(new.record_evidence(&old_pins, "null"), Err(EvidenceError::Stale));
    }
}
