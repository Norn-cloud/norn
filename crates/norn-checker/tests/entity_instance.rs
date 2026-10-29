#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "integration-test crate"
)]

//! #2848: effective object types for instance validation, built from a set of
//! `entity` artifacts with the whole-model check's own tables.

use norn_checker::{
    EffectiveCatalog, EffectiveKind, EntityArtifactSetContext, InstanceError, KeyComponent,
    check_artifact_set,
};
use norn_ir::{ArtifactEnvelope, PropertyType};
use serde_json::{Value, json};

const CATALOG: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/commerce.property-types.entity.json");
const INTERFACE: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/party.interface.entity.json");
const SUPPLIER: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/purchasing-supplier.entity.json");
const SKU: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/retail-sku.entity.json");
const EXTENSION: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/acme-sku.extension.entity.json");
const UNKNOWN_SUPERTYPE: &str = include_str!(
    "../../../fixtures/ir/entity-v2/artifact-set/invalid/unknown-supertype.entity.json"
);

fn envelopes(sources: &[&str]) -> Vec<ArtifactEnvelope> {
    sources
        .iter()
        .map(|source| serde_json::from_str(source).expect("fixture should decode"))
        .collect()
}

#[test]
fn ref_and_interface_constraints_conjoin_on_the_effective_property() {
    let catalog = EffectiveCatalog::from_artifacts(&envelopes(&[CATALOG, INTERFACE, SUPPLIER]));
    let supplier = catalog
        .object_type("purchasing.Supplier.entity.v2")
        .expect("the supplier artifact defines an object type")
        .expect("the supplier resolves");
    assert_eq!(supplier.unsupported(), None);
    assert_eq!(supplier.interfaces[0].as_str(), "party.Party");
    let display_name = supplier
        .property("display_name")
        .expect("display_name is effective");
    assert_eq!(
        display_name.kind,
        EffectiveKind::Base(PropertyType::Text { format: None })
    );
    // Its own ref chain (party.display_name) and the interface requirement.
    assert_eq!(display_name.constraints.len(), 3);
    display_name
        .check_value(&json!("Acme"))
        .expect("a name within every bound is admitted");
    assert!(matches!(
        display_name.check_value(&json!("")),
        Err(InstanceError::ConstraintViolation(_))
    ));
    let code = supplier
        .property("supplier_code")
        .expect("supplier_code is effective");
    code.check_value(&json!("S-000123"))
        .expect("a matching code is admitted");
    assert!(matches!(
        code.check_value(&json!("S-12")),
        Err(InstanceError::ConstraintViolation(_))
    ));
    assert_eq!(
        supplier.keys[0].value(
            supplier,
            json!({ "supplier_code": "S-000123" })
                .as_object()
                .expect("object")
        ),
        Some(vec![KeyComponent::Text("S-000123".to_owned())])
    );
    assert_eq!(
        catalog
            .lineage("purchasing.Supplier")
            .map(<[norn_ir::ObjectTypeName]>::len),
        Some(1)
    );
}

#[test]
fn property_and_computed_markings_accumulate_from_shared_types_and_interfaces() {
    let artifacts = envelopes(&[
        r#"{
          "irVersion":"0.1","artifactId":"test.markings","kind":"entity",
          "namespace":"test","name":"Markings","version":"0.1.0","lifecycle":"active",
          "scope":{"orgId":"org_demo"},"dependencies":[],"provenance":{"source":"authoring_sdk"},
          "payload":{"entityVersion":"2.0","markings":[
            {"id":"pii.contact","label":"Contact","severity":"moderate","category":"pii.contact","mask":{"kind":"redacted"}},
            {"id":"confidential.finance","label":"Finance","severity":"high","category":"confidential.finance","mask":{"kind":"redacted"}}
          ]}
        }"#,
        r#"{
          "irVersion":"0.1","artifactId":"test.property-types","kind":"entity",
          "namespace":"test","name":"Property types","version":"0.1.0","lifecycle":"active",
          "scope":{"orgId":"org_demo"},"dependencies":[],"provenance":{"source":"authoring_sdk"},
          "payload":{"entityVersion":"2.0","propertyTypes":[
            {"name":"test.email","type":{"kind":"text","format":"email"},"markings":["pii.contact"]}
          ]}
        }"#,
        r#"{
          "irVersion":"0.1","artifactId":"test.contact-interface","kind":"entity",
          "namespace":"test","name":"Contact","version":"0.1.0","lifecycle":"active",
          "scope":{"orgId":"org_demo"},"dependencies":[],"provenance":{"source":"authoring_sdk"},
          "payload":{"entityVersion":"2.0","interface":{
            "name":"test.Contact","markings":["confidential.finance"],"properties":[
              {"id":"email","type":{"kind":"ref","name":"test.email"}}
            ]
          }}
        }"#,
        r#"{
          "irVersion":"0.1","artifactId":"test.person","kind":"entity",
          "namespace":"test","name":"Person","version":"0.1.0","lifecycle":"active",
          "scope":{"orgId":"org_demo"},"dependencies":[],"provenance":{"source":"authoring_sdk"},
          "payload":{"entityVersion":"2.0","objectType":{
            "name":"test.Person","implements":["test.Contact"],"properties":[
              {"id":"email","type":{"kind":"ref","name":"test.email"}},
              {"id":"email_copy","type":{"kind":"text"},"computed":"self.email"},
              {"id":"email_derived","type":{"kind":"text"},"computed":"self.email_copy"}
            ]
          }}
        }"#,
    ]);
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let person = catalog
        .object_type("test.person")
        .expect("person artifact defines an object type")
        .expect("markings do not require catalog data in effective-model construction");
    let expected = [
        norn_ir::MarkingId("confidential.finance".to_owned()),
        norn_ir::MarkingId("pii.contact".to_owned()),
    ];
    for property in ["email", "email_copy", "email_derived"] {
        assert_eq!(
            person
                .property(property)
                .expect("property is effective")
                .markings,
            expected,
            "computed dependency propagation should retain every upstream marking"
        );
    }
    assert!(
        person
            .property("email")
            .expect("stored property")
            .computed_inputs
            .is_empty()
    );
    assert_eq!(
        person
            .property("email_copy")
            .expect("computed property")
            .computed_inputs,
        vec!["email".to_owned()]
    );
    assert_eq!(
        person
            .property("email_derived")
            .expect("transitively computed property")
            .computed_inputs,
        vec!["email_copy".to_owned()]
    );
}

#[test]
fn extension_members_and_links_are_part_of_the_effective_type() {
    let catalog = EffectiveCatalog::from_artifacts(&envelopes(&[
        CATALOG, INTERFACE, SUPPLIER, SKU, EXTENSION,
    ]));
    let sku = catalog
        .object_type("retail.Sku.entity.v2")
        .expect("the sku artifact defines an object type")
        .expect("a tenant-scoped extension does not block the type");
    assert!(sku.property("acme__shelf_label").is_some());
    assert!(sku.has_links);
    let unsupported = sku.unsupported().expect("links fail closed");
    assert_eq!(unsupported.path, "artifact.payload.objectType.links");
}

#[test]
fn a_finding_blocks_only_the_types_that_depend_on_its_artifact() {
    let catalog = EffectiveCatalog::from_artifacts(&envelopes(&[
        CATALOG,
        INTERFACE,
        SUPPLIER,
        UNKNOWN_SUPERTYPE,
    ]));
    let blocked = catalog
        .object_type("retail.SeasonalSku.entity.v2")
        .expect("the artifact defines an object type")
        .expect_err("an unknown supertype makes the type unavailable");
    assert_eq!(
        blocked.code.as_str(),
        "object_type.validation.unknown_supertype"
    );
    catalog
        .object_type("purchasing.Supplier.entity.v2")
        .expect("the supplier artifact defines an object type")
        .expect("an unrelated finding does not block the supplier");
    assert!(catalog.object_type("party.Party.interface.v2").is_none());
}

#[test]
fn historical_oversized_type_and_link_names_remain_projectable_but_not_admissible() {
    let long_type_name = format!("test.{}", "T".repeat(300));
    let long_link_id = format!("historical_{}", "l".repeat(280));
    let long_type = entity(
        "test.long-type.entity.v2",
        &json!({
            "entityVersion": "2.0",
            "objectType": { "name": long_type_name, "properties": [] }
        }),
    );
    let customer = entity(
        "test.customer.entity.v2",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.Customer",
                "properties": [],
                "links": [{
                    "id": long_link_id,
                    "target": long_type_name,
                    "cardinality": { "to": "one", "from": "many" }
                }]
            }
        }),
    );
    let artifacts = [long_type, customer];

    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    catalog
        .object_type_named(&long_type_name)
        .expect("historical type is indexed")
        .expect("admission-only byte limits do not hide retained contracts");
    let customer_type = catalog
        .object_type_named("test.Customer")
        .expect("customer type is indexed")
        .expect("its historical oversized link remains readable");
    assert!(customer_type.has_links);
    assert_eq!(
        catalog
            .links_for_object_type("test.Customer")
            .expect("customer links remain projected")[0]
            .id
            .as_str(),
        long_link_id
    );

    let registry = std::collections::BTreeSet::new();
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    );
    assert!(
        findings
            .iter()
            .any(|finding| { finding.code.as_str() == "identifier.validation.too_long" })
    );

    let mut invalid_customer = serde_json::to_value(&artifacts[1]).expect("artifact JSON");
    invalid_customer["payload"]["objectType"]["links"][0]["id"] =
        json!(format!("bad-id-{}", "l".repeat(280)));
    let invalid_customer: ArtifactEnvelope =
        serde_json::from_value(invalid_customer).expect("artifact remains decodable");
    let catalog = EffectiveCatalog::from_artifacts(&[artifacts[0].clone(), invalid_customer]);
    assert_eq!(
        catalog
            .object_type_named("test.Customer")
            .expect("customer type is indexed")
            .expect_err("invalid link grammar still blocks the effective type")
            .code
            .as_str(),
        "object_type.validation.invalid_link_id"
    );
}

#[test]
fn defaults_are_materialized_in_instance_encoding() {
    let artifact: Value = json!({
        "irVersion": "0.1.0",
        "artifactId": "retail.Bin.entity.v2",
        "kind": "entity",
        "namespace": "retail",
        "name": "Bin",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": { "orgId": "org_demo" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.Bin",
                "properties": [
                    { "id": "capacity", "type": { "kind": "integer" }, "constraints": { "min": "1" }, "default": "10" },
                    { "id": "meta", "type": { "kind": "json-ext" }, "default": { "slots": 4, "ratio": 0.5 } },
                    { "id": "label", "type": { "kind": "text" }, "computed": "self.capacity" }
                ]
            }
        }
    });
    let catalog = EffectiveCatalog::from_artifacts(&[
        serde_json::from_value(artifact).expect("envelope should decode")
    ]);
    let bin = catalog
        .object_type("retail.Bin.entity.v2")
        .expect("the artifact defines an object type")
        .expect("the type resolves");
    let default = |id: &str| {
        bin.property(id)
            .expect("property is effective")
            .default_value()
            .expect("default materializes")
    };
    assert_eq!(default("capacity"), Some(json!(10)));
    assert_eq!(default("meta"), Some(json!({ "slots": 4, "ratio": 0.5 })));
    assert_eq!(default("label"), None);
}

fn entity(artifact_id: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1.0",
        "artifactId": artifact_id,
        "kind": "entity",
        "namespace": "test",
        "name": artifact_id,
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": { "orgId": "org_demo" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": payload
    }))
    .expect("envelope should decode")
}

fn catalog(artifact_id: &str, property_types: &Value) -> ArtifactEnvelope {
    entity(
        artifact_id,
        &json!({ "entityVersion": "2.0", "propertyTypes": property_types }),
    )
}

/// A catalog definition whose `ref` resolves nowhere (`unknown_ref`).
fn broken(name: &str) -> Value {
    json!({ "name": name, "type": { "kind": "ref", "name": "missing.name" } })
}

fn sku_type(property_type: &str) -> ArtifactEnvelope {
    entity(
        "type.sku",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "retail.Sku",
                "properties": [{ "id": "code", "type": { "kind": "ref", "name": property_type } }]
            }
        }),
    )
}

fn availability(artifacts: &[ArtifactEnvelope], artifact_id: &str) -> Result<(), String> {
    EffectiveCatalog::from_artifacts(artifacts)
        .object_type(artifact_id)
        .expect("the artifact defines an object type")
        .map(|_| ())
        .map_err(|finding| {
            format!(
                "{} in {}",
                finding.code.as_str(),
                finding.artifact_id.clone().unwrap_or_default()
            )
        })
}

#[test]
fn a_finding_in_an_unrelated_catalog_does_not_block_a_type() {
    let a = catalog(
        "catalog.a",
        &json!([{ "name": "commerce.sku_code", "type": { "kind": "text" } }]),
    );
    // Catalog B has an unknown ref and duplicates a name catalog E defines;
    // neither touches commerce.sku_code.
    let b = catalog(
        "catalog.b",
        &json!([broken("other.thing"), { "name": "other.dup", "type": { "kind": "text" } }]),
    );
    let e = catalog(
        "catalog.e",
        &json!([{ "name": "other.dup", "type": { "kind": "integer" } }]),
    );
    assert_eq!(
        availability(&[a, b, e, sku_type("commerce.sku_code")], "type.sku"),
        Ok(())
    );
}

#[test]
fn a_finding_in_a_referenced_catalog_or_on_the_ref_chain_blocks_a_type() {
    let chain_end = |extra: Vec<Value>| {
        let mut defs = vec![json!({ "name": "base.code", "type": { "kind": "text" } })];
        defs.extend(extra);
        catalog("catalog.c", &Value::Array(defs))
    };
    let a = |extra: Vec<Value>| {
        let mut defs = vec![
            json!({ "name": "commerce.sku_code", "type": { "kind": "text" } }),
            json!({ "name": "commerce.short_sku", "type": { "kind": "ref", "name": "base.code" }, "constraints": { "maxLength": 6 } }),
        ];
        defs.extend(extra);
        catalog("catalog.a", &Value::Array(defs))
    };

    // An error in the catalog that defines the referenced type.
    assert_eq!(
        availability(
            &[
                a(vec![broken("commerce.bad")]),
                sku_type("commerce.sku_code")
            ],
            "type.sku"
        ),
        Err("property_type.validation.unknown_ref in catalog.a".to_owned())
    );
    // The chain commerce.short_sku (A) -> base.code (C): clean, then an
    // error in C, which the type reads only through the chain.
    assert_eq!(
        availability(
            &[
                a(Vec::new()),
                chain_end(Vec::new()),
                sku_type("commerce.short_sku")
            ],
            "type.sku"
        ),
        Ok(())
    );
    assert_eq!(
        availability(
            &[
                a(Vec::new()),
                chain_end(vec![broken("base.bad")]),
                sku_type("commerce.short_sku")
            ],
            "type.sku"
        ),
        Err("property_type.validation.unknown_ref in catalog.c".to_owned())
    );
    // A referenced name that a second catalog also defines is ambiguous.
    let duplicate = catalog(
        "catalog.d",
        &json!([{ "name": "commerce.sku_code", "type": { "kind": "text" } }]),
    );
    assert_eq!(
        availability(
            &[a(Vec::new()), duplicate, sku_type("commerce.sku_code")],
            "type.sku"
        ),
        Err("property_type.validation.duplicate_name in catalog.d".to_owned())
    );
    // An interface requirement's ref counts too (a property meeting a `ref`
    // requirement narrows through the same chain).
    let interface = entity(
        "interface.party",
        &json!({
            "entityVersion": "2.0",
            "interface": {
                "name": "party.Party",
                "properties": [{ "id": "display_name", "type": { "kind": "ref", "name": "base.code" } }]
            }
        }),
    );
    let party = entity(
        "type.party",
        &json!({
            "entityVersion": "2.0",
            "objectType": {
                "name": "crm.Customer",
                "implements": ["party.Party"],
                "properties": [{ "id": "display_name", "type": { "kind": "ref", "name": "base.code" } }]
            }
        }),
    );
    assert_eq!(
        availability(
            &[chain_end(vec![broken("base.bad")]), interface, party],
            "type.party"
        ),
        Err("property_type.validation.unknown_ref in catalog.c".to_owned())
    );
}

#[test]
fn record_values_are_atomic_closed_and_exact() {
    let definition: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name":"test.Point", "properties":[{"id":"point", "type":{"kind":"record","members":[
            {"id":"latitude","type":{"kind":"decimal","scale":6},"constraints":{"min":"-90","max":"90"}},
            {"id":"longitude","type":{"kind":"decimal","scale":6},"constraints":{"min":"-180","max":"180"}}
        ]},"constraints":{"nullable":true}}]
    })).expect("definition");
    let object = norn_checker::EffectiveObjectType::standalone(&definition).expect("standalone");
    let property = object.property("point").expect("point");
    for good in ["null", r#"{"latitude":30.123456,"longitude":31.234567}"#] {
        let value = norn_ir::RuntimeValue::from_json_text(good).expect("exact value");
        property
            .check_runtime_value(&value)
            .expect("complete point admitted");
        let encoded = norn_checker::property_codec::encode(norn_ir::PropertyKind::Record, &value)
            .expect("encode");
        assert_eq!(
            norn_checker::property_codec::decode(norn_ir::PropertyKind::Record, &encoded)
                .expect("decode"),
            value
        );
    }
    for bad in [
        r#"{"latitude":30}"#,
        r#"{"latitude":null,"longitude":31}"#,
        r#"{"latitude":91,"longitude":31}"#,
        r#"{"latitude":30,"longitude":31,"extra":1}"#,
        r#"{"latitude":30.1234567,"longitude":31}"#,
    ] {
        assert!(
            property
                .check_runtime_value(&norn_ir::RuntimeValue::from_json_text(bad).expect("value"))
                .is_err(),
            "{bad}"
        );
    }
}

#[test]
fn record_text_constraints_check_the_persisted_nfc_representation() {
    for nested in [false, true] {
        for constraints in [json!({"minLength":2}), json!({"pattern":"^..$"})] {
            let leaf = json!({"id":"name","type":{"kind":"text"},"constraints":constraints});
            let member = if nested {
                json!({"id":"detail","type":{"kind":"record","members":[leaf]}})
            } else {
                leaf
            };
            let declaration = serde_json::from_value(json!({
                "id":"data","type":{"kind":"record","members":[member]}
            }))
            .expect("declaration");
            let property = norn_checker::entity_instance::EffectiveProperty::from_declared_property(
                &declaration,
                &[],
            );
            let wire = if nested {
                json!({"detail":{"name":"e\u{301}"}})
            } else {
                json!({"name":"e\u{301}"})
            };
            let runtime = norn_ir::RuntimeValue::from_json(wire.clone()).expect("runtime");
            assert!(property.check_value(&wire).is_err());
            assert!(property.check_runtime_value(&runtime).is_err());
        }
    }

    for (property_type, constraints) in [
        (
            json!({"kind":"text"}),
            json!({"maxLength":1,"pattern":"^é$"}),
        ),
        (json!({"kind":"enum","values":[{"value":"é"}]}), json!({})),
    ] {
        let declaration = serde_json::from_value(json!({
            "id":"data","type":{"kind":"record","members":[{
                "id":"name","type":property_type,"constraints":constraints
            }]}
        }))
        .expect("declaration");
        let property = norn_checker::entity_instance::EffectiveProperty::from_declared_property(
            &declaration,
            &[],
        );
        let wire = json!({"name":"e\u{301}"});
        let runtime = norn_ir::RuntimeValue::from_json(wire.clone()).expect("runtime");
        property
            .check_value(&wire)
            .expect("NFC-equivalent wire value");
        property
            .check_runtime_value(&runtime)
            .expect("NFC-equivalent runtime value");
        let encoded = norn_checker::property_codec::encode(norn_ir::PropertyKind::Record, &runtime)
            .expect("encode");
        let stored = norn_checker::property_codec::decode(norn_ir::PropertyKind::Record, &encoded)
            .expect("decode");
        assert_eq!(
            stored,
            norn_ir::RuntimeValue::from_json(json!({"name":"é"})).expect("NFC value")
        );
        property
            .check_runtime_value(&stored)
            .expect("the persisted value still satisfies the same contract");
    }
}

#[test]
fn record_member_refs_enforce_catalog_constraints_and_exact_defaults() {
    let mut types: Value = serde_json::from_str(CATALOG).expect("catalog envelope");
    types["payload"]["propertyTypes"] = json!([
        {"name":"test.coordinate","type":{"kind":"decimal","scale":6},"constraints":{"min":"-90","max":"90"}},
        {"name":"test.point","type":{"kind":"record","members":[
            {"id":"latitude","type":{"kind":"ref","name":"test.coordinate"}},
            {"id":"note","type":{"kind":"text"},"constraints":{"nullable":true}}
        ]}}
    ]);
    let mut object: Value = serde_json::from_str(SKU).expect("object envelope");
    object["payload"]["objectType"] = json!({"name":"retail.Sku","properties":[{
        "id":"point","type":{"kind":"ref","name":"test.point"},
        "default":{"latitude":"30.123456","note":null}
    }]});
    let artifacts: Vec<ArtifactEnvelope> = [types, object]
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("envelope"))
        .collect();
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let object = catalog
        .object_type("retail.Sku.entity.v2")
        .expect("object")
        .expect("valid catalog");
    let property = object.property("point").expect("point");
    let default = property
        .runtime_default_literal(&property.default.as_ref().expect("default").0)
        .expect("exact default");
    property
        .check_runtime_value(&default)
        .expect("default is admitted");
    let invalid =
        norn_ir::RuntimeValue::from_json_text(r#"{"latitude":91,"note":null}"#).expect("value");
    assert!(property.check_runtime_value(&invalid).is_err());
}
