#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use norn_ir::{ActionRoot, ObjectTypeDef, PropertyTypeDef};
use norn_projections::{
    CedarAttribute, CedarLoss, CedarPrincipalAttribute, project_cedar_schema,
    project_cedar_schema_with_catalog, project_cedar_schema_with_catalog_and_principal_attributes,
};

const MINIMAL_GOLDEN: &str = "projections/cedar/minimal.cedarschema";
const REPRESENTATIVE_ATTRIBUTES_GOLDEN: &str = "projections/cedar/representative.attributes.txt";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn object_type(value: serde_json::Value) -> ObjectTypeDef {
    serde_json::from_value(value).expect("object type decodes")
}

fn property_type(value: serde_json::Value) -> PropertyTypeDef {
    serde_json::from_value(value).expect("property type decodes")
}

fn action(value: serde_json::Value) -> ActionRoot {
    serde_json::from_value(value).expect("action decodes")
}

fn attributes_text(
    attributes: &std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, CedarAttribute>,
    >,
) -> String {
    let mut lines = Vec::new();
    for (entity_type, projected) in attributes {
        for (name, attribute) in projected {
            let value = match attribute {
                CedarAttribute::Bool => "Bool".to_owned(),
                CedarAttribute::Long => "Long".to_owned(),
                CedarAttribute::String => "String".to_owned(),
                CedarAttribute::Decimal => "Decimal".to_owned(),
                CedarAttribute::Link { many } => format!("Link {{ many: {many} }}"),
            };
            lines.push(format!("{entity_type}.{name} = {value}"));
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn representative_model() -> (Vec<ObjectTypeDef>, Vec<PropertyTypeDef>, Vec<ActionRoot>) {
    let types = vec![
        object_type(serde_json::json!({
            "name": "alpha.Root",
            "properties": [
                { "id": "code", "type": { "kind": "ref", "name": "shared.code" } },
                { "id": "enabled", "type": { "kind": "boolean" } },
                { "id": "count", "type": { "kind": "integer" } },
                { "id": "total", "type": { "kind": "decimal", "scale": 2 } },
                { "id": "payload", "type": { "kind": "json-ext" } },
                { "id": "scope", "type": { "kind": "text" } },
                { "id": "acted_by", "type": { "kind": "text" } }
            ],
            "links": [
                {
                    "id": "parent",
                    "target": "alpha.Target",
                    "cardinality": { "to": "one", "from": "many" },
                    "inverse": "children",
                    "properties": [
                        { "id": "rank", "type": { "kind": "integer" } }
                    ]
                },
                {
                    "id": "thing",
                    "target": "alpha.NamedThing",
                    "cardinality": { "to": "many", "from": "one" },
                    "inverse": "things"
                }
            ]
        })),
        object_type(serde_json::json!({
            "name": "alpha.Child",
            "extends": "alpha.Root",
            "properties": [
                { "id": "code", "type": { "kind": "enum", "values": [{ "value": "A" }] } }
            ]
        })),
        object_type(serde_json::json!({
            "name": "alpha.Target",
            "implements": ["alpha.NamedThing"],
            "properties": [
                { "id": "scope", "type": { "kind": "text" } }
            ]
        })),
        object_type(serde_json::json!({
            "name": "alpha.SpecialTarget",
            "extends": "alpha.Target"
        })),
    ];
    let catalog = vec![
        property_type(serde_json::json!({
            "name": "shared.code",
            "type": { "kind": "ref", "name": "shared.code_base" }
        })),
        property_type(serde_json::json!({
            "name": "shared.code_base",
            "type": { "kind": "text", "format": "uuid" },
            "constraints": { "maxLength": 32, "pattern": "^[A-Z]+$" }
        })),
    ];
    let actions = vec![action(serde_json::json!({
        "actionId": "alpha.update",
        "scopeRequired": ["org"],
        "idempotent": false,
        "ledgered": false,
        "inputSchema": {},
        "outputSchema": {},
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": "id",
            "objectKind": "alpha.Root",
            "entityArtifactId": "alpha.Root",
            "entityArtifactVersion": "1.0.0"
        }
    }))];
    (types, catalog, actions)
}

#[test]
fn minimal_schema_matches_its_golden() {
    let object_types = vec![object_type(serde_json::json!({ "name": "demo.Note" }))];
    let projection = project_cedar_schema(&object_types, &[], &[]);
    let golden = std::fs::read_to_string(fixtures().join(MINIMAL_GOLDEN))
        .expect("minimal Cedar golden exists");
    assert_eq!(projection.schema, golden);
    assert!(projection.losses.is_empty());
    assert!(projection.attributes["demo.Note"].is_empty());
    assert!(!projection.attributes.contains_key("Norn::Actor"));
}

#[test]
fn principal_attribute_projection_emits_only_safe_typed_actor_fields() {
    let object_types = vec![object_type(serde_json::json!({ "name": "demo.Note" }))];
    let attributes = BTreeMap::from([
        ("business_unit".to_owned(), CedarPrincipalAttribute::String),
        (
            "business_units".to_owned(),
            CedarPrincipalAttribute::StringSet,
        ),
        ("enabled".to_owned(), CedarPrincipalAttribute::Bool),
        ("counts".to_owned(), CedarPrincipalAttribute::LongSet),
    ]);
    let projection = project_cedar_schema_with_catalog_and_principal_attributes(
        &object_types,
        &[],
        &[],
        &[],
        &attributes,
    );

    assert_eq!(projection.principal_attributes, attributes);
    assert!(projection.schema.contains("business_unit?: String"));
    assert!(projection.schema.contains("business_units?: Set<String>"));
    assert!(projection.schema.contains("enabled?: Bool"));
    assert!(projection.schema.contains("counts?: Set<Long>"));

    let invalid = BTreeMap::from([
        ("scope".to_owned(), CedarPrincipalAttribute::String),
        ("represents".to_owned(), CedarPrincipalAttribute::String),
        ("acted_by".to_owned(), CedarPrincipalAttribute::String),
        ("bad-name".to_owned(), CedarPrincipalAttribute::String),
        (
            "x }; entity Escape {".to_owned(),
            CedarPrincipalAttribute::String,
        ),
    ]);
    let projection = project_cedar_schema_with_catalog_and_principal_attributes(
        &object_types,
        &[],
        &[],
        &[],
        &invalid,
    );
    assert!(projection.principal_attributes.is_empty());
    assert_eq!(projection.losses.len(), invalid.len());
    assert!(!projection.schema.contains("bad-name"));
    assert!(!projection.schema.contains("entity Escape"));
}

#[test]
fn inheritance_catalog_refs_links_and_actions_project_deterministically() {
    let (object_types, property_types, actions) = representative_model();
    let projection =
        project_cedar_schema_with_catalog(&object_types, &[], &property_types, &actions);

    assert!(projection.schema.contains("code?: String"));
    assert!(projection.schema.contains("enabled?: Bool"));
    assert!(projection.schema.contains("count?: Long"));
    assert!(projection.schema.contains("total?: String"));
    assert!(
        projection
            .schema
            .contains("parent?: { type: String, key: String }")
    );
    assert!(
        projection
            .schema
            .contains("thing?: Set<{ type: String, key: String }>")
    );
    assert!(
        projection
            .schema
            .contains("children?: Set<{ type: String, key: String }>")
    );
    assert!(
        projection
            .schema
            .contains("things?: { type: String, key: String }")
    );
    assert!(
        projection
            .schema
            .contains("action \"alpha.update\" appliesTo")
    );
    assert!(
        projection
            .schema
            .contains("resource: [alpha::Child, alpha::Root]")
    );
    assert_eq!(
        projection.resource_actions[&norn_ir::ObjectTypeName("alpha.Root".to_owned())],
        vec!["alpha.update".to_owned()]
    );
    assert_eq!(
        projection.resource_actions[&norn_ir::ObjectTypeName("alpha.Child".to_owned())],
        vec!["alpha.update".to_owned()]
    );
    assert!(
        !projection
            .resource_actions
            .contains_key(&norn_ir::ObjectTypeName("alpha.Target".to_owned()))
    );
    assert!(!projection.schema.contains("payload?:"));
    assert!(!projection.schema.contains("scope?:"));
    assert!(!projection.schema.contains("acted_by?: String"));
    assert_eq!(
        projection.attributes["alpha.Child"]["total"],
        CedarAttribute::Decimal
    );
    assert_eq!(
        projection.attributes["alpha.Root"]["parent"],
        CedarAttribute::Link { many: false }
    );
    assert_eq!(
        projection.attributes["alpha.Root"]["thing"],
        CedarAttribute::Link { many: true }
    );
    assert_eq!(
        projection.attributes["alpha.SpecialTarget"]["children"],
        CedarAttribute::Link { many: true }
    );
    assert_eq!(
        projection.attributes["alpha.SpecialTarget"]["things"],
        CedarAttribute::Link { many: false }
    );
    assert_eq!(
        attributes_text(&projection.attributes),
        std::fs::read_to_string(fixtures().join(REPRESENTATIVE_ATTRIBUTES_GOLDEN))
            .expect("representative Cedar attribute golden exists")
    );
    for projected in projection.attributes.values() {
        assert!(!projected.contains_key("scope"));
        assert!(!projected.contains_key("acted_by"));
    }
    assert!(!projection.attributes.contains_key("Norn::Actor"));

    let (mut reversed_types, mut reversed_catalog, mut reversed_actions) = representative_model();
    reversed_types.reverse();
    reversed_catalog.reverse();
    reversed_actions.reverse();
    assert_eq!(
        projection,
        project_cedar_schema_with_catalog(
            &reversed_types,
            &[],
            &reversed_catalog,
            &reversed_actions
        )
    );
    assert!(projection.losses.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn unsupported_semantics_and_reserved_names_are_reported() {
    let (object_types, property_types, actions) = representative_model();
    let projection =
        project_cedar_schema_with_catalog(&object_types, &[], &property_types, &actions);
    let has_loss = |subject: &str, reason: &str| {
        projection
            .losses
            .iter()
            .any(|loss| loss.subject == subject && loss.reason.contains(reason))
    };

    assert!(has_loss("alpha.Root.payload", "opaque JSON"));
    assert!(has_loss(
        "alpha.Root.total",
        "decimal scale and numeric comparison"
    ));
    assert!(has_loss("alpha.Root.code", "text format constraint"));
    assert!(has_loss("alpha.Root.code", "text length or pattern"));
    assert!(has_loss("alpha.Root.scope", "reserved Cedar scope"));
    assert!(has_loss("alpha.Root.acted_by", "reserved Cedar scope"));
    assert!(has_loss("alpha.Target.scope", "reserved Cedar scope"));
    assert!(has_loss("alpha.Root.parent", "hierarchy marker"));
    assert!(has_loss("alpha.Root.parent.rank", "link property"));
    assert!(!has_loss("alpha.Root.thing", "minimum cardinality"));
    assert!(has_loss("alpha.update", "context values are not typed"));
    assert!(has_loss("alpha.update", "scope dimensions"));
    assert!(projection.losses.windows(2).all(|pair| pair[0] < pair[1]));
    let unique: BTreeSet<CedarLoss> = projection.losses.iter().cloned().collect();
    assert_eq!(unique.len(), projection.losses.len());
}

#[test]
fn missing_and_cyclic_property_refs_are_losses_and_omit_the_attribute() {
    let object_types = vec![object_type(serde_json::json!({
        "name": "demo.BadRefs",
        "properties": [
            { "id": "missing", "type": { "kind": "ref", "name": "shared.missing" } },
            { "id": "cycle", "type": { "kind": "ref", "name": "shared.a" } }
        ]
    }))];
    let catalog = vec![
        property_type(serde_json::json!({
            "name": "shared.a",
            "type": { "kind": "ref", "name": "shared.b" }
        })),
        property_type(serde_json::json!({
            "name": "shared.b",
            "type": { "kind": "ref", "name": "shared.a" }
        })),
    ];
    let projection = project_cedar_schema_with_catalog(&object_types, &[], &catalog, &[]);
    assert!(!projection.schema.contains("missing?:"));
    assert!(!projection.schema.contains("cycle?:"));
    assert!(projection.losses.iter().any(|loss| {
        loss.subject == "demo.BadRefs.missing -> shared.missing"
            && loss.reason.contains("absent from the supplied catalog")
    }));
    assert!(projection.losses.iter().any(|loss| {
        loss.subject == "demo.BadRefs.cycle -> shared.a"
            && loss.reason.contains("reference chain is cyclic")
    }));
}

#[test]
fn the_empty_catalog_wrapper_reports_refs_instead_of_guessing_their_type() {
    let object_types = vec![object_type(serde_json::json!({
        "name": "demo.Referenced",
        "properties": [
            { "id": "code", "type": { "kind": "ref", "name": "shared.code" } }
        ]
    }))];
    let projection = project_cedar_schema(&object_types, &[], &[]);
    assert!(!projection.schema.contains("code?:"));
    assert!(projection.losses.iter().any(|loss| {
        loss.subject == "demo.Referenced.code -> shared.code"
            && loss.reason.contains("absent from the supplied catalog")
    }));
}

#[test]
fn parent_interface_inverse_attributes_reach_child_interface_implementers() {
    let objects: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(serde_json::json!([
        {"name":"demo.Source","links":[{"id":"owner","target":"demo.Parent","inverse":"sources","cardinality":{"to":"one","from":"many"}}]},
        {"name":"demo.Target","implements":["demo.Child"]}
    ])).expect("objects");
    let interfaces: Vec<norn_ir::InterfaceDef> = serde_json::from_value(serde_json::json!([
        {"name":"demo.Parent"},{"name":"demo.Child","extends":["demo.Parent"]}
    ]))
    .expect("interfaces");
    let projection = project_cedar_schema(&objects, &interfaces, &[]);
    assert_eq!(
        projection.losses,
        vec![
            CedarLoss {
                subject: "demo.Source.owner".to_owned(),
                reason: "EntityIR has no hierarchy marker, so Cedar memberOf cannot be inferred",
            },
            CedarLoss {
                subject: "demo.Source.owner".to_owned(),
                reason: "link target type is not constrained by the generic Cedar record",
            },
            CedarLoss {
                subject: "demo.Target".to_owned(),
                reason: "implemented interface identity is not represented in Cedar",
            },
            CedarLoss {
                subject: "demo.Target.sources".to_owned(),
                reason: "inverse link target type is not constrained by the generic Cedar record",
            }
        ]
    );
    assert!(projection.schema.contains("sources?: Set<"));
}
