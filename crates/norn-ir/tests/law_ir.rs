#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "golden contract tests"
)]
use norn_ir::{ArtifactEnvelope, ArtifactKind, LawIR};
use serde_json::Value;

fn effective_instance() -> norn_ir::verification::EffectiveLawInstance {
    use norn_ir::definition_catalog::{Member, MemberRef};
    let at = |id: &str, member| MemberRef {
        artifact_id: id.to_owned(),
        version: "1.0.0".to_owned(),
        member,
    };
    let origin = at(
        "toy.capacity",
        Member::Type {
            type_name: "toy.Capacity".to_owned(),
        },
    );
    let subject = at(
        "toy.credit",
        Member::Type {
            type_name: "toy.Credit".to_owned(),
        },
    );
    norn_ir::verification::EffectiveLawInstance {
        law: at(
            "toy.capacity.law",
            Member::Law {
                law: "toy.preserve".to_owned(),
            },
        ),
        law_digest: "a".repeat(64),
        subject: subject.clone(),
        subject_digest: "b".repeat(64),
        attachment: norn_ir::LawRoot::Pack("toy.layer".to_owned()),
        origin: origin.clone(),
        paths: vec![vec![origin, subject.clone()]],
        instance_paths: Vec::new(),
        bindings: std::collections::BTreeMap::from([("self".to_owned(), subject)]),
    }
}

#[test]
fn law_instance_contract_keeps_identity_and_diamond_paths_separate_from_law_ir() {
    let mut instance = effective_instance();
    let mut other_path = instance.paths.first().expect("path").clone();
    other_path.insert(1, instance.law.clone());
    instance.paths.push(other_path);
    let wire = serde_json::to_value(&instance).expect("wire");
    assert_eq!(
        serde_json::from_value::<norn_ir::verification::EffectiveLawInstance>(wire.clone())
            .expect("instance"),
        instance
    );
    assert!(serde_json::from_value::<LawIR>(wire.clone()).is_err());
    assert_eq!(wire["paths"].as_array().expect("paths").len(), 2);
    for field in [
        "lawDigest",
        "subjectDigest",
        "origin",
        "paths",
        "instancePaths",
        "bindings",
    ] {
        let mut invalid = wire.clone();
        invalid.as_object_mut().expect("instance").remove(field);
        assert!(
            serde_json::from_value::<norn_ir::verification::EffectiveLawInstance>(invalid).is_err()
        );
    }
    let mut invalid = wire;
    invalid["proved"] = serde_json::json!(true);
    assert!(
        serde_json::from_value::<norn_ir::verification::EffectiveLawInstance>(invalid).is_err()
    );
}

proptest::proptest! {
    #[test]
    fn law_instance_generated_paths_and_substitutions_roundtrip(names in proptest::collection::vec("[a-z]{1,12}", 0..24)) {
        let mut instance = effective_instance();
        for name in names {
            let mut member = instance.subject.clone();
            member.artifact_id = format!("toy.{name}");
            instance.bindings.insert(name, member.clone());
            instance.paths.push(vec![instance.origin.clone(), member, instance.subject.clone()]);
        }
        let wire = serde_json::to_value(&instance).expect("wire");
        let restored: norn_ir::verification::EffectiveLawInstance = serde_json::from_value(wire).expect("instance");
        proptest::prop_assert_eq!(restored, instance);
    }
}

fn pack_reference(id: String) -> norn_ir::PackInstanceRef {
    norn_ir::PackInstanceRef {
        instance_id: id,
        pack: norn_ir::mapping::InterpretationEndpoint {
            artifact_id: "toy.pack".to_owned(),
            version: "1.0.0".to_owned(),
            sha256: "a".repeat(64),
        },
    }
}

#[test]
fn law_contained_contract_retains_exact_instance_paths_and_refuses_missing_pins() {
    let mut instance = effective_instance();
    instance.instance_paths = vec![vec![
        pack_reference("root".to_owned()),
        pack_reference("leaf".to_owned()),
    ]];
    let wire = serde_json::to_value(&instance).expect("wire");
    assert_eq!(
        serde_json::from_value::<norn_ir::verification::EffectiveLawInstance>(wire.clone())
            .expect("instance"),
        instance
    );
    for field in ["instanceId", "pack"] {
        let mut invalid = wire.clone();
        invalid["instancePaths"][0][1]
            .as_object_mut()
            .expect("instance ref")
            .remove(field);
        assert!(
            serde_json::from_value::<norn_ir::verification::EffectiveLawInstance>(invalid).is_err()
        );
    }
    let mut invalid = wire;
    invalid["instancePaths"][0][1]["pack"]
        .as_object_mut()
        .expect("pin")
        .remove("sha256");
    assert!(
        serde_json::from_value::<norn_ir::verification::EffectiveLawInstance>(invalid).is_err()
    );
}

proptest::proptest! {
    #[test]
    fn law_contained_generated_instance_paths_roundtrip(ids in proptest::collection::vec("[a-z]{1,12}", 1..16)) {
        let mut instance = effective_instance();
        instance.instance_paths = vec![ids.into_iter().map(pack_reference).collect()];
        let wire = serde_json::to_value(&instance).expect("wire");
        let restored: norn_ir::verification::EffectiveLawInstance = serde_json::from_value(wire).expect("instance");
        proptest::prop_assert_eq!(restored, instance);
    }
}

#[test]
fn laws_decode_losslessly_and_reject_open_temporal_shapes() {
    let goldens: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json")).expect("goldens");
    for golden in goldens {
        let envelope: ArtifactEnvelope = serde_json::from_value(golden.clone()).expect("envelope");
        assert_eq!(envelope.kind(), ArtifactKind::Law);
        assert_eq!(serde_json::to_value(envelope).expect("encoded"), golden);
        let law: LawIR = serde_json::from_value(golden["payload"].clone()).expect("law");
        assert_eq!(
            serde_json::to_value(law).expect("encoded law"),
            golden["payload"]
        );
        let mut invalid = golden["payload"].clone();
        invalid["modality"] = serde_json::json!({"kind":"always", "until":"true"});
        assert!(serde_json::from_value::<LawIR>(invalid).is_err());
        let mut invalid = golden["payload"].clone();
        invalid["condition"] = Value::Null;
        assert!(serde_json::from_value::<LawIR>(invalid).is_err());
    }
}
