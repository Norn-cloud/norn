#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! `EntityIR` v1 → v2 lowering goldens (T1-ONT-4, #2517).
//!
//! Every valid v1 `entity` and `relation` fixture under `fixtures/ir/` has a
//! golden lowering in `fixtures/ir/entity-v2/lowered/<fixture file name>`:
//! the lowered `EntityRootV2` for an entity, and `{ source, link }` for a
//! relation. Regenerate with
//! `NORN_CHECKER_UPDATE_GOLDENS=1 cargo test -p norn-checker --test entity_lowering`.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use norn_checker::entity_lowering::{
    ENTITY_V1_DEPRECATED, json_ext_primitive_type, lower_field_type_v1,
};
use norn_checker::{
    EffectiveObjectType, EntityForm, check_artifact_json, codes, decode_artifact_json,
    entity_object_type, entity_v1_deprecation, lower_cardinality_v1, lower_entity_v1,
    relation_link,
};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, DiagnosticSeverity, EntityFieldV1, EntityRoot, EntityRootV1,
    EntityRootV2, LinkCardinality, Multiplicity, PropertyType, RelationCardinalityV1,
};
use serde_json::{Value, json};

const UPDATE_ENV: &str = "NORN_CHECKER_UPDATE_GOLDENS";

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate sits two levels below the repo root")
}

fn lowered_dir() -> PathBuf {
    repo_root().join("fixtures/ir/entity-v2/lowered")
}

fn json_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("fixture directory should be readable") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            json_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            files.push(path);
        }
    }
}

fn read_json(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn envelope(path: &Path, value: &Value) -> ArtifactEnvelope {
    assert_eq!(
        check_artifact_json(value),
        Vec::new(),
        "{} is not a valid fixture",
        path.display()
    );
    decode_artifact_json(value)
        .unwrap_or_else(|diagnostics| panic!("{}: {diagnostics:?}", path.display()))
}

/// Every valid v1 `entity` and `relation` envelope fixture under
/// `fixtures/ir/` (the lowered goldens themselves are payloads, not
/// envelopes), with its lowering.
fn lowerings() -> Vec<(String, Value)> {
    let mut files = Vec::new();
    json_files(&repo_root().join("fixtures/ir"), &mut files);
    files.sort();
    let lowered = lowered_dir();
    let mut out = Vec::new();
    for path in files {
        if path.starts_with(&lowered) {
            continue;
        }
        let value = read_json(&path);
        let (Some(kind), Some(payload)) = (value.get("kind"), value.get("payload")) else {
            continue;
        };
        let name = path
            .file_name()
            .expect("fixture file name")
            .to_string_lossy()
            .into_owned();
        match kind.as_str() {
            Some("entity") if payload.get("entityVersion").is_none() => {
                let artifact = envelope(&path, &value);
                let ArtifactPayload::Entity(payload) = &artifact.payload else {
                    panic!("{name} is not an entity payload");
                };
                let lowered = entity_object_type(payload)
                    .expect("a valid v1 entity lowers")
                    .expect("a v1 entity defines an object type");
                assert_eq!(lowered.form, EntityForm::V1, "{name}");
                out.push((
                    name,
                    serde_json::to_value(&lowered.root).expect("lowered root serializes"),
                ));
            }
            Some("relation") => {
                let artifact = envelope(&path, &value);
                let ArtifactPayload::Relation(payload) = &artifact.payload else {
                    panic!("{name} is not a relation payload");
                };
                let lowered = relation_link(&artifact.name, payload).expect("a relation lowers");
                out.push((
                    name,
                    json!({ "source": lowered.source, "link": lowered.link }),
                ));
            }
            _ => {}
        }
    }
    out
}

#[test]
fn every_v1_entity_and_relation_fixture_has_a_current_lowering_golden() {
    let lowerings = lowerings();
    let names: BTreeSet<&str> = lowerings.iter().map(|(name, _)| name.as_str()).collect();
    assert!(
        names.contains("retail-sku.entity.json")
            && names.contains("retail-sku-supplier.relation.json"),
        "the v1 entity and relation fixtures were not discovered: {names:?}"
    );

    let dir = lowered_dir();
    let update = std::env::var_os(UPDATE_ENV).is_some();
    if update {
        fs::create_dir_all(&dir).expect("lowered golden directory should be creatable");
    }
    for (name, lowered) in &lowerings {
        let path = dir.join(name);
        if update {
            let mut rendered =
                serde_json::to_string_pretty(lowered).expect("golden should serialize");
            rendered.push('\n');
            fs::write(&path, rendered).expect("golden should be writable");
            continue;
        }
        assert!(
            path.exists() && read_json(&path) == *lowered,
            "{} is missing or stale; regenerate with `{UPDATE_ENV}=1 cargo test -p norn-checker --test entity_lowering`",
            path.display()
        );
    }

    // No golden outlives its fixture.
    let on_disk: BTreeSet<String> = fs::read_dir(&dir)
        .expect("lowered golden directory should be readable")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let expected: BTreeSet<String> = names.iter().map(|name| (*name).to_owned()).collect();
    assert_eq!(
        on_disk, expected,
        "fixtures/ir/entity-v2/lowered has stale goldens"
    );
}

#[test]
fn lowered_entity_goldens_decode_as_entity_ir_v2_object_type_roots() {
    for (name, lowered) in lowerings() {
        if !name.ends_with(".entity.json") {
            continue;
        }
        let root: EntityRootV2 = serde_json::from_value(lowered.clone())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            serde_json::to_value(&root).expect("serializes"),
            lowered,
            "{name}"
        );
        // The typed `entity` root selects the v2 object type form for it.
        assert!(
            matches!(serde_json::from_value(lowered), Ok(EntityRoot::V2(_))),
            "{name} does not select the v2 objectType form"
        );
    }
}

#[test]
fn retail_sku_v1_lowers_to_the_documented_v2_shape() {
    let v1: EntityRootV1 = serde_json::from_value(json!({
        "objectKind": "retail.Sku",
        "fields": { "skuId": { "type": "text", "required": true } }
    }))
    .expect("v1 root decodes");
    assert_eq!(
        serde_json::to_value(lower_entity_v1(&v1)).expect("serializes"),
        json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.Sku",
                "properties": [{ "id": "skuId", "type": { "kind": "text" } }]
            }
        })
    );
}

#[test]
fn v1_type_and_field_markings_survive_lowering_and_accumulate_effectively() {
    let v1: EntityRootV1 = serde_json::from_value(json!({
        "objectKind": "demo.Person",
        "markings": ["pii.contact"],
        "fields": {
            "email": { "type": "text", "required": true, "markings": ["pii.identity"] },
            "name": { "type": "text", "required": true }
        }
    }))
    .expect("marked V1 entity decodes");
    let lowered = lower_entity_v1(&v1);
    assert_eq!(
        lowered.object_type.markings,
        [norn_ir::MarkingId("pii.contact".to_owned())]
    );
    assert_eq!(
        lowered.object_type.properties[0].markings,
        [norn_ir::MarkingId("pii.identity".to_owned())]
    );
    let effective = EffectiveObjectType::standalone(&lowered.object_type)
        .expect("lowered V1 type resolves standalone");
    assert_eq!(
        effective
            .property("email")
            .expect("email is effective")
            .markings,
        [
            norn_ir::MarkingId("pii.contact".to_owned()),
            norn_ir::MarkingId("pii.identity".to_owned())
        ]
    );
    assert_eq!(
        effective
            .property("name")
            .expect("name is effective")
            .markings,
        [norn_ir::MarkingId("pii.contact".to_owned())]
    );
}

#[test]
fn every_v1_field_type_and_requiredness_lowers() {
    let v1: EntityRootV1 = serde_json::from_value(json!({
        "objectKind": "demo.Everything",
        "fields": {
            "a_text": { "type": "text", "required": true },
            "b_string": { "type": "string" },
            "c_integer": { "type": "integer", "required": false },
            "d_number": { "type": "number", "required": true },
            "e_boolean": { "type": "boolean" },
            "f_object": { "type": "object" },
            "g_array": { "type": "array", "required": true },
            "h_enum": { "type": "enum", "values": ["open", "closed"] }
        },
        "mutationAuthority": { "kind": "domain_engine", "domain": "inventory" },
        "overlaySources": ["demo.overlay"],
        "extensions": { "demo/1.0.0": { "note": true } }
    }))
    .expect("v1 root decodes");
    assert_eq!(
        serde_json::to_value(lower_entity_v1(&v1)).expect("serializes"),
        json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "demo.Everything",
                "properties": [
                    { "id": "a_text", "type": { "kind": "text" } },
                    { "id": "b_string", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                    { "id": "c_integer", "type": { "kind": "integer" }, "constraints": { "nullable": true } },
                    { "id": "d_number", "type": { "kind": "json-ext", "schema": { "type": "number" } } },
                    { "id": "e_boolean", "type": { "kind": "boolean" }, "constraints": { "nullable": true } },
                    { "id": "f_object", "type": { "kind": "json-ext", "schema": { "type": "object" } }, "constraints": { "nullable": true } },
                    { "id": "g_array", "type": { "kind": "json-ext", "schema": { "type": "array" } } },
                    {
                        "id": "h_enum",
                        "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] },
                        "constraints": { "nullable": true }
                    }
                ]
            },
            "mutationAuthority": { "kind": "domain_engine", "domain": "inventory" },
            "extensions": { "demo/1.0.0": { "note": true } }
        })
    );
}

#[test]
fn only_lowered_json_types_read_back_as_json_ext_primitives() {
    let required = Some(true);
    for (field, expected) in [
        (
            EntityFieldV1::Number {
                required,
                markings: Vec::new(),
            },
            Some("number"),
        ),
        (
            EntityFieldV1::Object {
                required,
                markings: Vec::new(),
            },
            Some("object"),
        ),
        (
            EntityFieldV1::Array {
                required,
                markings: Vec::new(),
            },
            Some("array"),
        ),
        (
            EntityFieldV1::Text {
                required,
                markings: Vec::new(),
            },
            None,
        ),
        (
            EntityFieldV1::Integer {
                required,
                markings: Vec::new(),
            },
            None,
        ),
    ] {
        assert_eq!(
            json_ext_primitive_type(&lower_field_type_v1(&field)),
            expected
        );
    }
    assert_eq!(
        json_ext_primitive_type(&PropertyType::JsonExt { schema: None }),
        None
    );
    let wider: PropertyType = serde_json::from_value(json!({
        "kind": "json-ext",
        "schema": { "type": "number", "minimum": 0 }
    }))
    .expect("json-ext decodes");
    assert_eq!(json_ext_primitive_type(&wider), None);
}

#[test]
fn relation_cardinality_follows_the_documented_table() {
    let link = |to, from| LinkCardinality {
        to,
        from,
        required: false,
    };
    assert_eq!(
        lower_cardinality_v1(Some(RelationCardinalityV1::OneToOne)),
        link(Multiplicity::One, Multiplicity::One)
    );
    assert_eq!(
        lower_cardinality_v1(Some(RelationCardinalityV1::OneToMany)),
        link(Multiplicity::Many, Multiplicity::One)
    );
    assert_eq!(
        lower_cardinality_v1(Some(RelationCardinalityV1::ManyToOne)),
        link(Multiplicity::One, Multiplicity::Many)
    );
    assert_eq!(
        lower_cardinality_v1(Some(RelationCardinalityV1::ManyToMany)),
        link(Multiplicity::Many, Multiplicity::Many)
    );
    assert_eq!(
        lower_cardinality_v1(None),
        link(Multiplicity::Many, Multiplicity::Many)
    );
}

#[test]
fn lowered_v1_relation_matches_the_link_the_v2_fixture_declares() {
    let relation_path = repo_root().join("fixtures/ir/retail-sku-supplier.relation.json");
    let relation = envelope(&relation_path, &read_json(&relation_path));
    let ArtifactPayload::Relation(payload) = &relation.payload else {
        panic!("relation fixture");
    };
    let lowered = relation_link(&relation.name, payload).expect("relation lowers");

    let entity_path = repo_root().join("fixtures/ir/retail-sku-v2.entity.json");
    let entity = envelope(&entity_path, &read_json(&entity_path));
    let ArtifactPayload::Entity(payload) = &entity.payload else {
        panic!("entity fixture");
    };
    let v2 = entity_object_type(payload)
        .expect("v2 decodes")
        .expect("object type");
    assert_eq!(v2.form, EntityForm::V2);
    assert_eq!(lowered.source, v2.root.object_type.name);
    let declared = v2
        .root
        .object_type
        .links
        .iter()
        .find(|link| link.id == lowered.link.id)
        .expect("the v2 fixture declares the lowered link id");
    assert_eq!(declared.target, lowered.link.target);
    assert_eq!(declared.cardinality, lowered.link.cardinality);
}

#[test]
fn v2_definition_forms_without_an_object_type_read_as_none() {
    let path = repo_root().join("fixtures/ir/entity-v2/artifact-set/party.interface.entity.json");
    let artifact = envelope(&path, &read_json(&path));
    let ArtifactPayload::Entity(payload) = &artifact.payload else {
        panic!("entity fixture");
    };
    assert_eq!(entity_object_type(payload), Ok(None));
}

#[test]
fn deprecation_is_a_registered_warning_on_v1_entities_only() {
    let entry = codes::lookup(ENTITY_V1_DEPRECATED).expect("registered");
    assert_eq!(entry.severity, DiagnosticSeverity::Warning);

    let v1_path = repo_root().join("fixtures/ir/retail-sku.entity.json");
    let v1 = envelope(&v1_path, &read_json(&v1_path));
    let warning = entity_v1_deprecation(&v1).expect("v1 entity is deprecated");
    assert_eq!(warning.code, ENTITY_V1_DEPRECATED);
    assert_eq!(warning.severity, DiagnosticSeverity::Warning);
    assert_eq!(warning.artifact_id.as_deref(), Some("retail.Sku.entity"));

    for other in [
        "fixtures/ir/retail-sku-v2.entity.json",
        "fixtures/ir/retail-sku-supplier.relation.json",
    ] {
        let path = repo_root().join(other);
        assert_eq!(
            entity_v1_deprecation(&envelope(&path, &read_json(&path))),
            None,
            "{other}"
        );
    }
}
