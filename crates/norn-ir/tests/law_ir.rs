#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
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

/// Every `LawIR` object (a JSON object with the law's required members) in
/// the repository's `registry/` and `fixtures/` trees, keyed by
/// `path#pointer`, in path order.
fn repository_laws() -> Vec<(String, Value)> {
    fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("readable directory")
            .map(|entry| entry.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                out.push(path);
            }
        }
    }
    fn walk(value: &Value, pointer: &str, key: &str, out: &mut Vec<(String, Value)>) {
        match value {
            Value::Object(map) => {
                if [
                    "lawVersion",
                    "id",
                    "subject",
                    "scope",
                    "modality",
                    "response",
                ]
                .iter()
                .all(|member| map.contains_key(*member))
                {
                    out.push((format!("{key}#{pointer}"), value.clone()));
                }
                for (name, child) in map {
                    let escaped = name.replace('~', "~0").replace('/', "~1");
                    walk(child, &format!("{pointer}/{escaped}"), key, out);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{pointer}/{index}"), key, out);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths = Vec::new();
    for tree in ["registry", "fixtures"] {
        files(&root.join(tree), &mut paths);
    }
    let mut laws = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).expect("readable fixture");
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let key = path
            .strip_prefix(&root)
            .expect("inside the repository")
            .to_string_lossy()
            .replace('\\', "/");
        walk(&value, "", &key, &mut laws);
    }
    laws
}

/// The optional exception tree changes no existing law: every law artifact
/// under `registry/` and `fixtures/` decodes, re-encodes to exactly the
/// canonical bytes of its source JSON, and keeps the Norn1 pin recorded in
/// `tests/golden/law-pins.json` (generated from the code before the field
/// existed). Regenerate only for a deliberate fixture change:
/// `NORN_UPDATE_LAW_PINS=1 cargo test -p norn-ir --test law_ir`.
#[test]
fn every_repository_law_keeps_its_canonical_bytes_and_pin() {
    use norn_ir::canonical::{HashProfile, canonical_json_of, canonicalize_json};
    let golden_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/law-pins.json");
    let mut pins = serde_json::Map::new();
    for (key, raw) in repository_laws() {
        let law: LawIR = serde_json::from_value(raw.clone())
            .unwrap_or_else(|error| panic!("{key} decodes as LawIR: {error}"));
        let source = canonicalize_json(serde_json::to_vec(&raw).expect("raw bytes"))
            .expect("canonical source");
        let encoded = canonical_json_of(&law).expect("canonical law");
        assert_eq!(encoded, source, "{key}: canonical bytes changed");
        pins.insert(
            key,
            Value::String(format!(
                "sha256:{}",
                HashProfile::Norn1.hash_canonical_bytes(&encoded)
            )),
        );
    }
    assert!(
        pins.len() >= 10,
        "found only {} laws; the scan is broken",
        pins.len()
    );
    let actual = Value::Object(pins);
    if std::env::var_os("NORN_UPDATE_LAW_PINS").is_some() {
        let mut text = serde_json::to_string_pretty(&actual).expect("pins");
        text.push('\n');
        std::fs::write(&golden_path, text).expect("write pins");
        return;
    }
    let golden: Value = serde_json::from_str(
        &std::fs::read_to_string(&golden_path).expect("golden pins are checked in"),
    )
    .expect("golden pins");
    assert_eq!(
        actual, golden,
        "a law's pin changed or a law was added or removed"
    );
}

fn excepted_law() -> Value {
    let goldens: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json")).expect("goldens");
    let mut law = goldens[0]["payload"].clone();
    let source =
        serde_json::json!({"artifactId": "erpnext.no_unpaid_ship.law", "pointer": "/payload"});
    law["exceptions"] = serde_json::json!([
        {"id": "prepaid", "overrides": law["id"], "condition": "self.prepaid == true",
         "rationale": "Prepaid orders ship.", "source": source},
        {"id": "fraud", "overrides": "prepaid", "condition": "self.flagged == true",
         "rationale": "A flagged prepaid order is not exempt.", "source": source}
    ]);
    law
}

#[test]
fn law_exceptions_round_trip_and_have_one_encoding() {
    use norn_ir::canonical::{canonical_json_of, canonicalize_json};
    let wire = excepted_law();
    let law: LawIR = serde_json::from_value(wire.clone()).expect("law with exceptions");
    assert_eq!(law.exceptions.len(), 2);
    assert_eq!(law.exceptions[1].overrides, "prepaid");
    assert_eq!(serde_json::to_value(&law).expect("encoded"), wire);
    assert_eq!(
        canonical_json_of(&law).expect("canonical"),
        canonicalize_json(serde_json::to_vec(&wire).expect("raw")).expect("canonical raw")
    );
    // Dropping the list gives back the original law and its bytes.
    let mut plain = law.clone();
    plain.exceptions.clear();
    let mut original = wire.clone();
    original
        .as_object_mut()
        .expect("object")
        .remove("exceptions");
    assert_eq!(serde_json::to_value(&plain).expect("encoded"), original);
    // An explicit empty list, null, or an unknown exception member is refused.
    for invalid in [serde_json::json!([]), Value::Null] {
        let mut law = wire.clone();
        law["exceptions"] = invalid;
        assert!(serde_json::from_value::<LawIR>(law).is_err());
    }
    let mut law = wire.clone();
    law["exceptions"][0]["consequence"] = serde_json::json!("set");
    assert!(serde_json::from_value::<LawIR>(law).is_err());
    let mut law = wire;
    law["exceptions"][0]
        .as_object_mut()
        .expect("object")
        .remove("overrides");
    assert!(serde_json::from_value::<LawIR>(law).is_err());
    // The published schema carries the field as optional and nonempty.
    let schema = serde_json::to_value(schemars::schema_for!(LawIR)).expect("schema");
    assert_eq!(schema["properties"]["exceptions"]["minItems"], 1);
    assert!(
        !schema["required"]
            .as_array()
            .expect("required")
            .contains(&serde_json::json!("exceptions"))
    );
}
