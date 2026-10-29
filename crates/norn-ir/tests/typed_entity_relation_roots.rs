#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! T1-IR-1b: typed Entity (`EntityIR` v1 or v2) and Relation (v1) roots.
//!
//! Every entity and relation fixture decodes through the lenient envelope root
//! *and* through the strict typed root, and both re-encode to the same
//! canonical JSON: the v1 wire is unchanged, and v2 is a new, separately
//! versioned form (`entityVersion: "2.0"`) whose `objectType` is the `EntityIR`
//! v2 [`ObjectTypeDef`].

use norn_canonical::HashProfile;
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, EntityAuthorityDomain, EntityFieldV1,
    EntityMutationAuthority, EntityRoot, EntityRootV1, EntityRootV2, JsonValue, Multiplicity,
    ObjectTypeDef, RelationCardinalityV1, RelationRoot,
};
use serde_json::{Value, json};

const V1_ENTITY_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-sku.entity.json");
const V2_ENTITY_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-sku-v2.entity.json");
const RELATION_FIXTURE: &str =
    include_str!("../../../fixtures/ir/retail-sku-supplier.relation.json");
const SKU_OBJECT_TYPE: &str =
    include_str!("../../../fixtures/ir/entity-v2/object-types/sku.object-type.json");

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

fn decodes<T: serde::de::DeserializeOwned>(payload: Value) -> bool {
    serde_json::from_value::<T>(payload).is_ok()
}

fn v1_payload() -> Value {
    json!({
        "objectKind": "commerce.Order",
        "fields": {
            "orderId": { "type": "text", "required": true },
            "status": { "type": "enum", "values": ["open", "closed"] },
            "total": { "type": "number" }
        },
        "mutationAuthority": { "kind": "domain_engine", "domain": "accounting" },
        "overlaySources": ["customization.test.add_field"]
    })
}

#[test]
fn v1_entity_fixture_decodes_into_the_v1_root() {
    let payload = payload_json(V1_ENTITY_FIXTURE);
    let root: EntityRoot = assert_typed_roundtrip("retail-sku.entity.json", &payload);
    let EntityRoot::V1(v1) = &root else {
        panic!("a payload without entityVersion is EntityIR v1: {root:?}");
    };
    assert_eq!(root.object_kind(), Some("retail.Sku"));
    assert_eq!(
        v1.fields["skuId"],
        EntityFieldV1::Text {
            required: Some(true),
            markings: Vec::new()
        }
    );
    assert!(v1.fields["skuId"].is_required());

    let full: EntityRootV1 = assert_typed_roundtrip("v1 payload", &v1_payload());
    assert_eq!(
        full.mutation_authority,
        Some(EntityMutationAuthority::DomainEngine {
            domain: EntityAuthorityDomain::Accounting
        })
    );
    assert_eq!(
        full.fields["status"],
        EntityFieldV1::Enum {
            required: None,
            markings: Vec::new(),
            values: vec!["open".into(), "closed".into()]
        }
    );
    assert!(!full.fields["total"].is_required());
}

#[test]
fn v1_root_and_fields_can_declare_markings() {
    let payload = json!({
        "objectKind": "party.Contact",
        "markings": ["pii.contact"],
        "fields": {
            "email": {
                "type": "text",
                "required": true,
                "markings": ["pii.identity"]
            }
        }
    });
    let root: EntityRoot = assert_typed_roundtrip("marked v1 payload", &payload);
    let EntityRoot::V1(root) = &root else {
        panic!("a payload without entityVersion is EntityIR v1: {root:?}");
    };
    assert_eq!(root.markings[0].as_str(), "pii.contact");
    assert_eq!(root.fields["email"].markings()[0].as_str(), "pii.identity");
}

#[test]
fn v2_entity_fixture_decodes_into_the_v2_root() {
    let payload = payload_json(V2_ENTITY_FIXTURE);
    let root: EntityRoot = assert_typed_roundtrip("retail-sku-v2.entity.json", &payload);
    let EntityRoot::V2(v2) = &root else {
        panic!("a payload with entityVersion is EntityIR v2: {root:?}");
    };
    assert_eq!(root.object_kind(), Some("retail.Sku"));
    assert!(root.mutation_authority().is_none());

    // The object type is exactly the EntityIR v2 definition, plus the link
    // that replaces the v1 relation fixture.
    let mut expected: ObjectTypeDef =
        serde_json::from_str(SKU_OBJECT_TYPE).expect("sku object type should decode");
    assert!(expected.links.is_empty());
    expected.links = v2.object_type.links.clone();
    assert_eq!(v2.object_type, expected);
    let [link] = v2.object_type.links.as_slice() else {
        panic!("the v2 fixture declares one link");
    };
    assert_eq!(link.id.as_str(), "preferred_supplier");
    assert_eq!(link.target.as_str(), "purchasing.Supplier");
    assert_eq!(link.cardinality.to, Multiplicity::One);
    assert_eq!(link.cardinality.from, Multiplicity::Many);

    // The lenient envelope root keeps the v2 wire bytes and knows the form.
    let envelope: ArtifactEnvelope =
        serde_json::from_str(V2_ENTITY_FIXTURE).expect("v2 envelope should decode");
    let ArtifactPayload::Entity(lenient) = &envelope.payload else {
        panic!("entity kind selects the entity root");
    };
    assert!(lenient.is_entity_ir_v2());
    assert!(lenient.unknown_fields().is_empty());
    let ArtifactPayload::Entity(v1) = &serde_json::from_str::<ArtifactEnvelope>(V1_ENTITY_FIXTURE)
        .expect("v1 envelope should decode")
        .payload
    else {
        panic!("entity kind selects the entity root");
    };
    assert!(!v1.is_entity_ir_v2());
}

#[test]
fn owned_collection_row_bounds_decode_after_the_ir_json_value_roundtrip() {
    let payload = json!({
        "entityVersion": "2.0",
        "objectType": {
            "name": "test.Parent",
            "ownedCollections": [{
                "id": "lines",
                "childType": "test.Line",
                "rowIdProperty": "row_id",
                "orderingKey": "idx",
                "cascadeOnParentDelete": true,
                "maxRows": 3
            }]
        }
    });
    let intermediate: JsonValue = serde_json::from_value(payload).expect("IR JSON value");
    let reserialized = serde_json::to_value(intermediate).expect("IR value serializes");
    let root: EntityRootV2 =
        serde_json::from_value(reserialized).expect("integral row bound decodes as a whole number");
    assert_eq!(root.object_type.owned_collections[0].max_rows.0, 3);
}

#[test]
fn relation_fixture_decodes_into_the_relation_root() {
    let payload = payload_json(RELATION_FIXTURE);
    let root: RelationRoot = assert_typed_roundtrip("retail-sku-supplier.relation.json", &payload);
    assert_eq!(root.from, "retail.Sku");
    assert_eq!(root.to, "purchasing.Supplier");
    assert_eq!(root.cardinality, Some(RelationCardinalityV1::ManyToOne));

    let minimal: RelationRoot =
        assert_typed_roundtrip("minimal relation", &json!({ "from": "a.A", "to": "b.B" }));
    assert_eq!(minimal.cardinality, None);
}

#[test]
fn the_two_entity_forms_are_disjoint_and_closed() {
    let v2 = payload_json(V2_ENTITY_FIXTURE);
    assert!(decodes::<EntityRootV2>(v2.clone()));
    assert!(!decodes::<EntityRootV1>(v2.clone()));

    // v1 members are not v2 members, and vice versa.
    for (member, value) in [
        ("objectKind", json!("retail.Sku")),
        ("fields", json!({})),
        ("overlaySources", json!([])),
        ("unknown", json!(true)),
    ] {
        let mut payload = v2.clone();
        payload[member] = value;
        assert!(!decodes::<EntityRoot>(payload), "v2 with {member}");
    }
    for (member, value) in [
        ("entityVersion", json!("2.0")),
        ("objectType", v2["objectType"].clone()),
        ("unknown", json!(true)),
    ] {
        let mut payload = v1_payload();
        payload[member] = value;
        assert!(!decodes::<EntityRootV1>(payload), "v1 with {member}");
    }

    // Only the `2.0` version literal selects v2.
    let mut payload = v2.clone();
    payload["entityVersion"] = json!("1.0");
    assert!(!decodes::<EntityRoot>(payload));
    let mut payload = v2;
    payload
        .as_object_mut()
        .expect("object")
        .remove("objectType");
    assert!(!decodes::<EntityRoot>(payload));

    // v1 field definitions are closed per type: `values` only on `enum`.
    let mut payload = v1_payload();
    payload["fields"]["total"]["values"] = json!(["x"]);
    assert!(!decodes::<EntityRootV1>(payload));
    let mut payload = v1_payload();
    payload["fields"]["total"]["type"] = json!("float");
    assert!(!decodes::<EntityRootV1>(payload));
    let mut payload = v1_payload();
    payload["mutationAuthority"] = json!({ "kind": "generic_action", "domain": "tax" });
    assert!(!decodes::<EntityRootV1>(payload));

    let mut relation = payload_json(RELATION_FIXTURE);
    relation["entityVersion"] = json!("2.0");
    assert!(
        !decodes::<RelationRoot>(relation),
        "there is no v2 relation form: v2 relations are links"
    );
}

#[test]
fn optional_members_may_be_omitted_but_not_null() {
    let v2 = payload_json(V2_ENTITY_FIXTURE);
    for (member, value) in [
        ("mutationAuthority", json!({ "kind": "generic_action" })),
        (
            "extensions",
            json!({ "example.com/audit/1.0.0": { "enabled": true } }),
        ),
    ] {
        let mut payload = v2.clone();
        payload[member] = value;
        assert!(decodes::<EntityRootV2>(payload.clone()), "v2 {member}");
        payload[member] = Value::Null;
        assert!(!decodes::<EntityRootV2>(payload), "v2 {member}: null");
    }
    let mut payload = v2;
    payload["objectType"]["description"] = Value::Null;
    assert!(!decodes::<EntityRootV2>(payload), "objectType members too");

    for member in ["mutationAuthority", "extensions"] {
        let mut payload = v1_payload();
        payload[member] = Value::Null;
        assert!(!decodes::<EntityRootV1>(payload), "v1 {member}: null");
    }
    let mut payload = v1_payload();
    payload["fields"]["total"]["required"] = Value::Null;
    assert!(!decodes::<EntityRootV1>(payload), "v1 required: null");
    // `overlaySources` is a list of customization artifact ids (T1-IR-2):
    // omitted or a string list is valid, `null` or any other JSON is not.
    let mut payload = v1_payload();
    payload["overlaySources"] = json!([]);
    assert!(decodes::<EntityRootV1>(payload.clone()));
    for invalid in [
        Value::Null,
        json!("customization.test.add_field"),
        json!([7]),
    ] {
        payload["overlaySources"] = invalid;
        assert!(!decodes::<EntityRootV1>(payload.clone()));
    }

    for member in ["cardinality", "extensions"] {
        let mut relation = payload_json(RELATION_FIXTURE);
        relation[member] = Value::Null;
        assert!(
            !decodes::<RelationRoot>(relation),
            "relation {member}: null"
        );
    }
}

const SET_CATALOG: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/commerce.property-types.entity.json");
const SET_INTERFACE: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/party.interface.entity.json");
const SET_EXTENSION: &str =
    include_str!("../../../fixtures/ir/entity-v2/artifact-set/acme-sku.extension.entity.json");

#[test]
fn v2_definition_forms_decode_into_their_roots_and_are_disjoint() {
    // #2844: interface, extension, and property type catalog forms.
    let interface = payload_json(SET_INTERFACE);
    let root: EntityRoot = assert_typed_roundtrip("party.interface.entity.json", &interface);
    let EntityRoot::V2Interface(form) = &root else {
        panic!("an interface member selects the interface form: {root:?}");
    };
    assert_eq!(form.interface.name.as_str(), "party.Party");
    assert_eq!(root.object_kind(), None);
    assert!(root.mutation_authority().is_none());

    let extension = payload_json(SET_EXTENSION);
    let root: EntityRoot = assert_typed_roundtrip("acme-sku.extension.entity.json", &extension);
    let EntityRoot::V2Extension(form) = &root else {
        panic!("an objectTypeExtension member selects the extension form: {root:?}");
    };
    assert_eq!(form.object_type_extension.target.as_str(), "retail.Sku");
    assert_eq!(root.object_kind(), None);

    let catalog = payload_json(SET_CATALOG);
    let root: EntityRoot = assert_typed_roundtrip("commerce.property-types.entity.json", &catalog);
    let EntityRoot::V2PropertyTypes(form) = &root else {
        panic!("a propertyTypes member selects the catalog form: {root:?}");
    };
    assert_eq!(form.property_types.len(), 2);
    assert_eq!(root.object_kind(), None);

    let markings = json!({
        "entityVersion": "2.0",
        "markings": [{
            "id": "pii.contact",
            "label": "Contact",
            "severity": "moderate",
            "category": "pii.contact",
            "mask": {"kind": "redacted"}
        }]
    });
    let root: EntityRoot = assert_typed_roundtrip("markings", &markings);
    let EntityRoot::V2Markings(form) = &root else {
        panic!("a markings member selects the markings catalog form: {root:?}");
    };
    assert_eq!(form.markings[0].id.as_str(), "pii.contact");
    assert_eq!(root.object_kind(), None);

    // Exactly one definition member: two do not decode, and neither does an
    // object type's mutation authority on another form.
    let object_type = payload_json(V2_ENTITY_FIXTURE)["objectType"].clone();
    for (mut payload, member, value) in [
        (interface.clone(), "objectType", object_type),
        (
            extension.clone(),
            "interface",
            interface["interface"].clone(),
        ),
        (
            catalog.clone(),
            "objectTypeExtension",
            extension["objectTypeExtension"].clone(),
        ),
        (
            interface,
            "mutationAuthority",
            json!({ "kind": "generic_action" }),
        ),
        (catalog, "fields", json!({})),
    ] {
        payload[member] = value;
        assert!(!decodes::<EntityRoot>(payload), "{member}");
    }

    // The lenient envelope root knows every member.
    for source in [SET_INTERFACE, SET_EXTENSION, SET_CATALOG] {
        let envelope: ArtifactEnvelope =
            serde_json::from_str(source).expect("envelope should decode");
        let ArtifactPayload::Entity(lenient) = &envelope.payload else {
            panic!("entity kind selects the entity root");
        };
        assert!(lenient.is_entity_ir_v2());
        assert!(lenient.unknown_fields().is_empty());
    }
}
