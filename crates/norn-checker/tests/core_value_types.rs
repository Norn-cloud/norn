#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "registry correspondence and invariant admission tests"
)]

#[path = "../../../registry/core/src/value_types.rs"]
mod value_types;

use std::collections::BTreeSet;

use norn_checker::{
    ArtifactValidation, EntityArtifactSetContext, EntityContext, EntityModel, check_artifact_set,
    check_entity_model, check_property_type_defs, entity_instance::EffectiveProperty,
};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, ObjectTypeDef, Property, PropertyConstraints, PropertyId,
    PropertyType, PropertyTypeCatalogRootV2, PropertyTypeDef, PropertyTypeName, RuntimeValue,
};
use serde_json::json;

fn extract_catalog(pack: ArtifactEnvelope) -> Vec<PropertyTypeDef> {
    pack.validate().expect("ordinary entity admission");
    let ArtifactPayload::Entity(payload) = pack.payload else {
        panic!("entity catalog")
    };
    let root: PropertyTypeCatalogRootV2 =
        serde_json::from_str(&serde_json::to_string(&payload).expect("typed root"))
            .expect("catalog root");
    root.property_types
}

fn combined_catalog() -> Vec<PropertyTypeDef> {
    let mut catalog = extract_catalog(value_types::core_pack().expect("core value types"));
    catalog.extend(extract_catalog(
        value_types::std_pack().expect("std value types"),
    ));
    catalog
}

fn property(name: &str, catalog: &[PropertyTypeDef]) -> EffectiveProperty {
    EffectiveProperty::from_declared_property(
        &Property {
            id: PropertyId("value".to_owned()),
            description: None,
            property_type: PropertyType::Ref {
                name: PropertyTypeName(name.to_owned()),
                additivity: norn_ir::PropertyAdditivity::Undeclared {},
                unit_source: Vec::new(),
            },
            constraints: PropertyConstraints::default(),
            default: None,
            markings: Vec::new(),
            computed: None,
            protected: false,
        },
        catalog,
    )
}

fn assert_generated_catalog(relative_path: &str, pack: ArtifactEnvelope) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    let exact = RuntimeValue::from_json(serde_json::to_value(&pack).expect("wire"))
        .expect("exact runtime value");
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&exact).expect("serializes")
    );
    if std::env::var_os("NORN_CHECKER_UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, &rendered).expect("regenerate");
    }
    assert_eq!(
        std::fs::read_to_string(&path).expect("generated catalog"),
        rendered,
        "{relative_path} drifted from registry/core/src/value_types.rs"
    );
    let defs = extract_catalog(pack);
    assert_eq!(check_property_type_defs(&defs, &BTreeSet::new()), []);
}

#[test]
fn core_and_std_value_types_match_generated_admitted_catalogs() {
    assert_generated_catalog(
        "../../registry/core/artifacts/core.value_types.entity.json",
        value_types::core_pack().expect("core pack"),
    );
    assert_generated_catalog(
        "../../registry/core/artifacts/std.value_types.entity.json",
        value_types::std_pack().expect("std pack"),
    );
}

#[test]
fn materialized_value_types_validate_scalars_and_iso4217_codes() {
    let catalog = combined_catalog();

    let currency = property("iso4217.currency_code", &catalog);
    for valid in ["USD", "EUR", "EGP", "JPY", "GBP", "XAU"] {
        assert!(
            currency
                .check_runtime_value(&RuntimeValue::from(valid))
                .is_ok(),
            "active ISO 4217 code {valid} should be admitted"
        );
    }
    for invalid in ["", "US", "USDD", "usd", "XXX", "XTS", "123"] {
        assert!(
            currency
                .check_runtime_value(&RuntimeValue::from(invalid))
                .is_err(),
            "non-active or malformed currency code {invalid} must be refused"
        );
    }

    let unit = property("ucum.unit_code", &catalog);
    assert!(unit.check_runtime_value(&RuntimeValue::from("kg")).is_ok());
    assert!(
        unit.check_runtime_value(&RuntimeValue::from("{each}"))
            .is_ok()
    );
    assert!(unit.check_runtime_value(&RuntimeValue::from("")).is_err());
    assert!(
        unit.check_runtime_value(&RuntimeValue::from("k g"))
            .is_err()
    );

    let quantity = property("core.quantity", &catalog);
    assert!(
        quantity
            .check_runtime_value(&RuntimeValue::from_json(json!(12.345_678)).expect("decimal"))
            .is_ok()
    );
    assert!(
        quantity
            .check_runtime_value(&RuntimeValue::from_json(json!(12.345_678_9)).expect("decimal"))
            .is_err()
    );

    let date = property("iso8601.date", &catalog);
    assert!(
        date.check_runtime_value(&RuntimeValue::from("2026-11-23"))
            .is_ok()
    );
    assert!(
        date.check_runtime_value(&RuntimeValue::from("2026-11-23T00:00:00Z"))
            .is_err()
    );
}

#[test]
fn core_rea_interfaces_admit_with_materialized_value_types_and_pinned_dependencies() {
    let base = vec![
        value_types::core_pack().expect("core"),
        value_types::std_pack().expect("std"),
    ];
    let artifacts: Vec<ArtifactEnvelope> = [
        include_str!("../../../registry/core/artifacts/core.Agent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.AgentRole.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Resource.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Commitment.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Hold.interface.json"),
        include_str!("../../../registry/core/artifacts/core.EconomicEvent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.LedgerEffect.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Claim.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Conversion.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Exchange.interface.json"),
    ]
    .into_iter()
    .map(|raw| serde_json::from_str(raw).expect("REA interface"))
    .collect();

    let registry = BTreeSet::new();
    let findings = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    );
    assert_eq!(
        findings,
        [],
        "REA interfaces must admit cleanly over materialized core/std value types"
    );
}

#[test]
fn unbodied_and_non_scalar_refs_remain_fail_closed_in_invariants() {
    let catalog = combined_catalog();
    let registry = BTreeSet::from([PropertyTypeName("external.unbodied_code".to_owned())]);

    let unbodied: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.UnbodiedInvariant",
        "properties": [
            { "id": "ext", "type": { "kind": "ref", "name": "external.unbodied_code" } }
        ],
        "invariants": [
            { "id": "reads_ext", "expression": "self.ext != ''", "message": "must be non-empty" }
        ]
    }))
    .expect("unbodied object type");

    let findings = check_entity_model(
        &EntityModel {
            object_types: &[unbodied],
            ..EntityModel::EMPTY
        },
        &EntityContext {
            catalog: &catalog,
            registry: &registry,
            base: EntityModel::EMPTY,
            tenant: None,
        },
    );
    assert!(
        findings.iter().any(|d| d.code.as_str()
            == "object_type.validation.invalid_invariant_expression"
            && d.message.contains("opaque")),
        "unbodied registry ref must remain fail-closed (opaque): {findings:?}"
    );

    let non_scalar: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.NonScalarInvariant",
        "properties": [
            { "id": "health", "type": { "kind": "ref", "name": "core.health_information" } }
        ],
        "invariants": [
            { "id": "reads_health", "expression": "self.health != null", "message": "must exist" }
        ]
    }))
    .expect("non-scalar object type");

    let findings = check_entity_model(
        &EntityModel {
            object_types: &[non_scalar],
            ..EntityModel::EMPTY
        },
        &EntityContext {
            catalog: &catalog,
            registry: &BTreeSet::new(),
            base: EntityModel::EMPTY,
            tenant: None,
        },
    );
    assert!(
        findings.iter().any(|d| d.code.as_str()
            == "object_type.validation.invalid_invariant_expression"
            && d.message.contains("opaque")),
        "non-scalar json-ext ref must remain fail-closed (opaque): {findings:?}"
    );
}

#[test]
fn single_object_invariant_scope_still_refuses_cross_object_link_reads() {
    let catalog = combined_catalog();
    let parent: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.Owner",
        "properties": [
            { "id": "code", "type": { "kind": "ref", "name": "core.code" } }
        ]
    }))
    .expect("owner");
    let child: ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.Item",
        "properties": [
            { "id": "quantity", "type": { "kind": "ref", "name": "core.quantity" } }
        ],
        "links": [
            { "id": "owner", "target": "test.Owner", "cardinality": { "to": "one", "from": "many", "required": true } }
        ],
        "invariants": [
            { "id": "cross_owner", "expression": "self.quantity > 0 and self.owner.code != ''", "message": "cross-object must refuse" }
        ]
    }))
    .expect("item");

    let findings = check_entity_model(
        &EntityModel {
            object_types: &[parent, child],
            ..EntityModel::EMPTY
        },
        &EntityContext {
            catalog: &catalog,
            registry: &BTreeSet::new(),
            base: EntityModel::EMPTY,
            tenant: None,
        },
    );
    assert!(
        findings.iter().any(|d| d.code.as_str()
            == "object_type.validation.invalid_invariant_expression"
            && d.message.contains("reads a link or inverse role")),
        "cross-object invariant must remain refused: {findings:?}"
    );
}
