#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! T1-IR-1e: typed Setting and `BusinessPack` (composition and ontology) roots.
//!
//! Every fixture of these kinds decodes through the lenient envelope root *and*
//! through the strict typed root, and both re-encode to the same canonical
//! JSON. The two new envelope fixtures pin their Norn1 canonical hashes.

use norn_canonical::HashProfile;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, BusinessPackRoot, CompositionPackRoot, MappingRelation,
    OntologyPackRoot, PackArtifactKind, SettingReviewState, SettingRoot, SettingValue,
    SettingValueType, SourceRedistribution,
};
use serde_json::{Value, json};

const SETTING_FIXTURE: &str =
    include_str!("../../../fixtures/ir/retail-reorder-threshold.setting.json");
const COMPOSITION_FIXTURE: &str =
    include_str!("../../../fixtures/ir/retail-core.business_pack.json");
const COMMERCE_ONTOLOGY_FIXTURE: &str =
    include_str!("../../../verification/ontology/packs/company-commerce-v0/pack.json");
const UNITS_ONTOLOGY_FIXTURE: &str =
    include_str!("../../../verification/ontology/packs/quantity-units-v0/pack.json");

fn canonical_hash(bytes: &[u8]) -> String {
    HashProfile::Norn1
        .hash_json(bytes)
        .expect("JSON should canonicalize")
}

fn payload_json(source: &str) -> Value {
    let envelope: Value = serde_json::from_str(source).expect("fixture should parse");
    envelope["payload"].clone()
}

/// Decode a payload into its typed root and assert the canonical JSON is
/// unchanged by the typed round-trip.
fn assert_typed_roundtrip<T>(name: &str, payload: &Value) -> T
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let typed: T = serde_json::from_value(payload.clone())
        .unwrap_or_else(|error| panic!("{name} should decode into its typed root: {error}"));
    let reencoded = serde_json::to_vec(&typed).expect("typed root should encode");
    let source = serde_json::to_vec(payload).expect("payload should encode");
    assert_eq!(
        canonical_hash(&reencoded),
        canonical_hash(&source),
        "{name} canonical JSON changed after typed round-trip"
    );
    typed
}

type Decode = fn(Value) -> bool;

fn decodes<T: serde::de::DeserializeOwned>(payload: Value) -> bool {
    serde_json::from_value::<T>(payload).is_ok()
}

/// Walk `path` (object keys, or array indices when numeric) from `root`.
fn at<'a>(root: &'a mut Value, path: &[&str]) -> &'a mut Value {
    let mut target = root;
    for segment in path {
        target = match segment.parse::<usize>() {
            Ok(index) => &mut target[index],
            Err(_) => &mut target[*segment],
        };
    }
    target
}

/// A JSON-valued setting with a `null` inside its value.
fn json_setting() -> Value {
    json!({
        "settingVersion": "0.1",
        "settingKey": "commerce.notification_preferences",
        "valueType": "json",
        "value": {
            "channels": ["email", "push"],
            "digest": { "enabled": true, "hour": 9 },
            "quietHours": null
        },
        "effectiveFrom": "2026-08-27T00:00:00Z",
        "review": { "state": "draft" },
        "scopeRequired": ["orgId"]
    })
}

#[test]
fn new_envelope_fixtures_roundtrip_with_pinned_hashes() {
    for (name, source, kind, pinned) in [
        (
            "fixtures/ir/retail-reorder-threshold.setting.json",
            SETTING_FIXTURE,
            ArtifactKind::Setting,
            "075143521b3c6891339b498040b5291853014513c9dca5db58c02aa8d3bca5d9",
        ),
        (
            "fixtures/ir/retail-core.business_pack.json",
            COMPOSITION_FIXTURE,
            ArtifactKind::BusinessPack,
            "ad97f54a764d7b6f4e0299d430cc3566c14a0b57300814c77b90a19f301e9f4a",
        ),
    ] {
        let envelope: ArtifactEnvelope = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
        assert_eq!(envelope.kind(), kind, "{name} kind");
        assert!(
            envelope.payload.unknown_fields().is_empty(),
            "{name} has undeclared members: {:?}",
            envelope.payload.unknown_fields()
        );
        let reserialized = serde_json::to_vec(&envelope).expect("envelope should serialize");
        let source_hash = canonical_hash(source.as_bytes());
        assert_eq!(
            canonical_hash(&reserialized),
            source_hash,
            "{name} canonical JSON changed after envelope round-trip"
        );
        assert_eq!(source_hash, pinned, "{name} canonical hash drifted");
    }
}

#[test]
fn setting_fixture_decodes_into_the_typed_root() {
    let setting: SettingRoot = assert_typed_roundtrip(
        "retail-reorder-threshold.setting.json",
        &payload_json(SETTING_FIXTURE),
    );
    assert_eq!(setting.value.value_type(), SettingValueType::Number);
    assert!(matches!(setting.value, SettingValue::Number(_)));
    assert_eq!(setting.review.state, SettingReviewState::Approved);
    assert_eq!(
        setting.supersedes.expect("fixture supersedes").version,
        "0.1.0"
    );

    let setting: SettingRoot = assert_typed_roundtrip("json setting", &json_setting());
    assert_eq!(setting.value.value_type(), SettingValueType::Json);

    // A `json` setting may hold `null`; a missing `value` does not decode.
    let mut null_value = json_setting();
    null_value["value"] = Value::Null;
    let _: SettingRoot = assert_typed_roundtrip("json setting with null value", &null_value);
    let mut missing_value = json_setting();
    missing_value
        .as_object_mut()
        .expect("payload object")
        .remove("value");
    assert!(!decodes::<SettingRoot>(missing_value));

    // `value` is typed by `valueType` (T1-IR-2): a disagreement does not
    // decode, and every value type round-trips with its own JSON type.
    for (value_type, value, expected) in [
        ("text", json!("reorder"), true),
        ("text", json!(5), false),
        ("number", json!(5.5), true),
        ("number", json!("5"), false),
        ("boolean", json!(true), true),
        ("boolean", Value::Null, false),
        ("json", json!({ "a": [1, null] }), true),
    ] {
        let mut setting = json_setting();
        setting["valueType"] = json!(value_type);
        setting["value"] = value;
        assert_eq!(
            decodes::<SettingRoot>(setting.clone()),
            expected,
            "{value_type}: {setting}"
        );
        if expected {
            let _: SettingRoot = assert_typed_roundtrip(value_type, &setting);
        }
    }

    // Nested objects keep the members the validator does not check.
    let mut open = payload_json(SETTING_FIXTURE);
    open["review"]["ticket"] = json!("OPS-12");
    open["supersedes"]["note"] = json!(null);
    let setting: SettingRoot = assert_typed_roundtrip("setting with open members", &open);
    assert!(setting.review.additional.contains_key("ticket"));
}

#[test]
fn composition_pack_fixture_and_legacy_form_decode_into_the_typed_root() {
    let pack: BusinessPackRoot = assert_typed_roundtrip(
        "retail-core.business_pack.json",
        &payload_json(COMPOSITION_FIXTURE),
    );
    let BusinessPackRoot::Composition(pack) = pack else {
        panic!("composition pack should select the composition form");
    };
    assert_eq!(pack.artifacts[3].kind, PackArtifactKind::Setting);

    // IR 0.1 composition packs predate `packKind`.
    let mut legacy = payload_json(COMPOSITION_FIXTURE);
    let object = legacy.as_object_mut().expect("payload object");
    object.remove("packKind");
    object.remove("provides");
    let legacy: BusinessPackRoot = assert_typed_roundtrip("legacy composition pack", &legacy);
    assert!(matches!(legacy, BusinessPackRoot::Composition(_)));

    // `derived` is not a composition-pack member kind.
    let mut derived = payload_json(COMPOSITION_FIXTURE);
    derived["artifacts"][0]["kind"] = json!("derived");
    assert!(!decodes::<CompositionPackRoot>(derived));

    // Artifact references keep the members the validator does not check.
    let mut open = payload_json(COMPOSITION_FIXTURE);
    open["artifacts"][0]["role"] = json!("root");
    let open: CompositionPackRoot = assert_typed_roundtrip("open artifact ref", &open);
    assert!(open.artifacts[0].additional.contains_key("role"));
}

#[test]
fn ontology_pack_fixtures_decode_into_the_typed_root() {
    for (name, source) in [
        ("company-commerce-v0", COMMERCE_ONTOLOGY_FIXTURE),
        ("quantity-units-v0", UNITS_ONTOLOGY_FIXTURE),
    ] {
        let pack: BusinessPackRoot = assert_typed_roundtrip(name, &payload_json(source));
        let BusinessPackRoot::Ontology(pack) = pack else {
            panic!("{name} should select the ontology form");
        };
        assert!(!pack.terms.is_empty() && !pack.shapes.is_empty(), "{name}");
    }
    let commerce: OntologyPackRoot = assert_typed_roundtrip(
        "company-commerce-v0",
        &payload_json(COMMERCE_ONTOLOGY_FIXTURE),
    );
    assert!(
        commerce
            .sources
            .iter()
            .any(|source| source.redistribution == SourceRedistribution::ReferenceOnly)
    );

    // An ontology pack is not a composition pack, and vice versa.
    assert!(!decodes::<CompositionPackRoot>(payload_json(
        COMMERCE_ONTOLOGY_FIXTURE
    )));
    assert!(!decodes::<OntologyPackRoot>(payload_json(
        COMPOSITION_FIXTURE
    )));
}

/// An exact mapping carries `ratifiedBy`/`ratifiedAt`; on the other relations
/// they are optional but still strings (T1-IR-2): `null` or a number does not
/// decode.
#[test]
fn ontology_mapping_ratification_members() {
    let mut pack = payload_json(UNITS_ONTOLOGY_FIXTURE);
    pack["mappings"][0]["relation"] = json!("exact_match");
    pack["mappings"][0]["ratifiedBy"] = json!("ontology_steward");
    pack["mappings"][0]["ratifiedAt"] = json!("2026-09-01T00:00:00Z");
    pack["mappings"][1]["ratifiedBy"] = json!("ontology_steward");
    let typed: OntologyPackRoot = assert_typed_roundtrip("ratified mappings", &pack);
    assert_eq!(typed.mappings[0].relation, MappingRelation::ExactMatch);
    assert_eq!(
        typed.mappings[1].ratified_by.as_deref(),
        Some("ontology_steward")
    );
    for invalid in [Value::Null, json!(7)] {
        let mut candidate = pack.clone();
        candidate["mappings"][1]["ratifiedAt"] = invalid;
        assert!(!decodes::<OntologyPackRoot>(candidate));
    }
}

/// Members the validator lets you omit but rejects when present as `null`
/// decode the same way: omitted is valid, explicit `null` is an error.
#[test]
fn optional_members_may_be_omitted_but_not_null() {
    let cases: [(&str, &[&str], &str, Decode); 12] = [
        (SETTING_FIXTURE, &[], "effectiveTo", decodes::<SettingRoot>),
        (SETTING_FIXTURE, &[], "supersedes", decodes::<SettingRoot>),
        (
            SETTING_FIXTURE,
            &["review"],
            "reviewedBy",
            decodes::<SettingRoot>,
        ),
        (
            SETTING_FIXTURE,
            &["review"],
            "reviewedAt",
            decodes::<SettingRoot>,
        ),
        (
            SETTING_FIXTURE,
            &["review"],
            "reason",
            decodes::<SettingRoot>,
        ),
        (
            COMPOSITION_FIXTURE,
            &[],
            "packKind",
            decodes::<CompositionPackRoot>,
        ),
        (
            COMPOSITION_FIXTURE,
            &[],
            "provides",
            decodes::<CompositionPackRoot>,
        ),
        (
            COMMERCE_ONTOLOGY_FIXTURE,
            &["sources", "0"],
            "files",
            decodes::<OntologyPackRoot>,
        ),
        (
            COMMERCE_ONTOLOGY_FIXTURE,
            &["terms", "1"],
            "parentTermId",
            decodes::<OntologyPackRoot>,
        ),
        (
            COMMERCE_ONTOLOGY_FIXTURE,
            &["terms", "0"],
            "supersededBy",
            decodes::<OntologyPackRoot>,
        ),
        (
            COMMERCE_ONTOLOGY_FIXTURE,
            &["mappings", "0"],
            "supersededByMappingId",
            decodes::<OntologyPackRoot>,
        ),
        (
            COMMERCE_ONTOLOGY_FIXTURE,
            &["shapes", "0", "fields", "0"],
            "targetTermId",
            decodes::<OntologyPackRoot>,
        ),
    ];
    for (source, parents, member, decode) in cases {
        let mut omitted = payload_json(source);
        if let Some(object) = at(&mut omitted, parents).as_object_mut() {
            object.remove(member);
        }
        assert!(decode(omitted.clone()), "{parents:?}.{member} omitted");

        let mut null = omitted;
        at(&mut null, parents)[member] = Value::Null;
        assert!(
            !decode(null),
            "{parents:?}.{member} = null must be rejected"
        );
    }
}

#[test]
fn typed_roots_are_closed_at_the_root() {
    let roots: [(&str, Decode); 4] = [
        (SETTING_FIXTURE, decodes::<SettingRoot>),
        (COMPOSITION_FIXTURE, decodes::<BusinessPackRoot>),
        (COMMERCE_ONTOLOGY_FIXTURE, decodes::<BusinessPackRoot>),
        (UNITS_ONTOLOGY_FIXTURE, decodes::<BusinessPackRoot>),
    ];
    for (source, decode) in roots {
        let mut payload = payload_json(source);
        assert!(decode(payload.clone()));
        payload["processId"] = json!("order_fulfillment");
        assert!(!decode(payload), "unknown root member must be rejected");
    }
    // Each form rejects the other form's members.
    let mut composition = payload_json(COMPOSITION_FIXTURE);
    composition["terms"] = json!([]);
    assert!(!decodes::<BusinessPackRoot>(composition));
    let mut ontology = payload_json(COMMERCE_ONTOLOGY_FIXTURE);
    ontology["provides"] = json!([]);
    assert!(!decodes::<BusinessPackRoot>(ontology));
}

/// The ontology validator rejects unknown members at every level, so every
/// ontology object is closed.
#[test]
fn ontology_pack_objects_are_closed() {
    for path in [
        &["sources", "0"][..],
        &["sources", "0", "files", "0"],
        &["terms", "0"],
        &["mappings", "0"],
        &["shapes", "0"],
        &["shapes", "0", "fields", "0"],
    ] {
        let mut pack = payload_json(COMMERCE_ONTOLOGY_FIXTURE);
        assert!(decodes::<OntologyPackRoot>(pack.clone()));
        at(&mut pack, path)["note"] = json!("unchecked");
        assert!(
            !decodes::<OntologyPackRoot>(pack),
            "{path:?}: unknown member must be rejected"
        );
    }
}

#[test]
fn null_extensions_are_rejected_by_the_typed_roots() {
    let roots: [(&str, Decode); 3] = [
        (SETTING_FIXTURE, decodes::<SettingRoot>),
        (COMPOSITION_FIXTURE, decodes::<BusinessPackRoot>),
        (COMMERCE_ONTOLOGY_FIXTURE, decodes::<BusinessPackRoot>),
    ];
    for (source, decode) in roots {
        let mut payload = payload_json(source);
        payload["extensions"] = json!({ "example.com/audit/1.0.0": { "enabled": true } });
        assert!(decode(payload.clone()));
        payload["extensions"] = Value::Null;
        assert!(!decode(payload), "extensions: null must be rejected");
    }
}
