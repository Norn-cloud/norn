#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "registry correspondence tests"
)]

#[path = "../../../registry/core/src/units.rs"]
mod units;

use std::collections::BTreeSet;

use norn_checker::{ArtifactValidation, entity_instance::EffectiveProperty};
use norn_ir::{
    ArtifactPayload, Property, PropertyConstraints, PropertyId, PropertyType, PropertyTypeDef,
    PropertyTypeName, RuntimeValue,
};
use proptest::prelude::*;

const MISMATCH: &str = "core.values.unit_mismatch";

fn catalog() -> Vec<PropertyTypeDef> {
    let pack = units::pack().expect("typed unit pack");
    pack.validate().expect("ordinary entity admission");
    let ArtifactPayload::Entity(payload) = pack.payload else {
        panic!("entity catalog")
    };
    let root: norn_ir::PropertyTypeCatalogRootV2 =
        serde_json::from_str(&serde_json::to_string(&payload).expect("typed root"))
            .expect("catalog root");
    root.property_types
}

fn property(name: &str) -> EffectiveProperty {
    EffectiveProperty::from_declared_property(
        &Property {
            id: PropertyId("unit".to_owned()),
            description: None,
            property_type: PropertyType::Ref {
                name: PropertyTypeName(format!("core.units.{name}")),
            },
            constraints: PropertyConstraints::default(),
            default: None,
            markings: Vec::new(),
            computed: None,
        },
        &catalog(),
    )
}

#[test]
fn core_units_is_the_exact_generated_admitted_catalog() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/core/artifacts/core.units.json");
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&units::pack().expect("pack")).expect("serializes")
    );
    if std::env::var_os("NORN_CHECKER_UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, &rendered).expect("regenerate");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("generated catalog"),
        rendered
    );
    assert!(norn_checker::check_property_type_defs(&catalog(), &BTreeSet::new()).is_empty());
}

#[test]
fn val_unit_closed_membership_refuses_unknown_units_and_money() {
    let code = property("code");
    for (value, _, _, _, _) in units::UNITS {
        assert!(code.check_runtime_value(&value.into()).is_ok());
    }
    for value in ["USD", "G", "m", "unit:KiloGM", "unknown"] {
        assert!(code.check_runtime_value(&value.into()).is_err());
    }
}

#[test]
fn val_unit_descriptor_refuses_another_unit_code() {
    let grams = property("g");
    let mut value = norn_ir::runtime_object! { "code" => "g", "dimension" => "mass", "numerator" => 1_i64, "denominator" => 1_i64 };
    assert!(grams.check_runtime_value(&value).is_ok());
    value
        .as_object_mut()
        .expect("record")
        .insert("code".to_owned(), "kg".into());
    assert!(grams.check_runtime_value(&value).is_err());
    for (_, _, _, numerator, denominator) in units::UNITS {
        assert!(numerator > 0 && denominator > 0);
    }
}

proptest! {
    #[test]
    fn val_unit_factor_and_dimension_cannot_be_substituted(index in 0..units::UNITS.len(), candidate in any::<i64>()) {
        let (code, name, dimension, numerator, denominator) = units::UNITS.get(index).copied().expect("bounded index");
        let constraint = property(name);
        let mut record = norn_ir::runtime_object! { "code" => code, "dimension" => dimension, "numerator" => numerator, "denominator" => denominator };
        prop_assert!(constraint.check_runtime_value(&record).is_ok());
        record.as_object_mut().expect("record").insert("numerator".to_owned(), candidate.into());
        prop_assert_eq!(constraint.check_runtime_value(&record).is_ok(), candidate == numerator);
        record.as_object_mut().expect("record").insert("numerator".to_owned(), numerator.into());
        record.as_object_mut().expect("record").insert("denominator".to_owned(), candidate.into());
        prop_assert_eq!(constraint.check_runtime_value(&record).is_ok(), candidate == denominator);
        record.as_object_mut().expect("record").insert("denominator".to_owned(), denominator.into());
        record.as_object_mut().expect("record").insert("dimension".to_owned(), RuntimeValue::from("currency"));
        prop_assert!(constraint.check_runtime_value(&record).is_err());
        let other_dimension = if dimension == "time" { "mass" } else { "time" };
        record.as_object_mut().expect("record").insert("dimension".to_owned(), other_dimension.into());
        prop_assert!(constraint.check_runtime_value(&record).is_err());
    }
    #[test]
    fn val_unit_admission_preserves_case_and_system(code in "[A-Za-z]{1,5}", ucum in any::<bool>()) {
        let system = if ucum { norn_ir::UnitSystem::Ucum } else { norn_ir::UnitSystem::Qudt };
        let member = norn_ir::RecordMember { id: PropertyId("amount".to_owned()), property_type: unit_type(&code, system), constraints: PropertyConstraints::default() };
        let artifact = declaration(PropertyType::Record { members: vec![member] });
        let findings = unit_findings(&artifact, &[units::pack().expect("pack")]);
        let expected = ucum && units::UNITS.iter().any(|unit| unit.0 == code);
        prop_assert_eq!(findings.is_empty(), expected);
        if !expected {
            prop_assert!(findings.iter().any(|d| d.code == MISMATCH && d.span.as_ref().is_some_and(|s| s.pointer == "/payload/propertyTypes/0/type/members/0/type/unit")));
        }
    }
}

fn unit_type(code: &str, system: norn_ir::UnitSystem) -> PropertyType {
    PropertyType::Decimal {
        scale: norn_ir::WholeNumber(2),
        unit: Some(norn_ir::UnitRef {
            system,
            code: code.to_owned(),
        }),
        currency_source: None,
        per_unit: None,
        rounding: None,
    }
}

fn declaration(ty: PropertyType) -> norn_ir::ArtifactEnvelope {
    let mut artifact = units::pack().expect("pack");
    "demo.units".clone_into(&mut artifact.artifact_id);
    "demo".clone_into(&mut artifact.namespace);
    artifact.dependencies = vec![norn_ir::ArtifactDependency {
        artifact_id: "core.units".to_owned(),
        kind: norn_ir::ArtifactKind::Entity,
        version: Some("1.0.0".to_owned()),
        required: true,
    }];
    let mut def = catalog().into_iter().next().expect("definition");
    def.name = PropertyTypeName("demo.amount".to_owned());
    def.property_type = ty;
    let root = norn_ir::PropertyTypeCatalogRootV2 {
        entity_version: norn_ir::EntityIrVersion::V2_0,
        property_types: vec![def],
        extensions: None,
    };
    artifact.payload = ArtifactPayload::Entity(
        serde_json::from_str(&serde_json::to_string(&root).expect("typed root")).expect("payload"),
    );
    artifact
}

fn unit_findings(
    artifact: &norn_ir::ArtifactEnvelope,
    base: &[norn_ir::ArtifactEnvelope],
) -> Vec<norn_checker::Diagnostic> {
    norn_checker::check_artifact_set(
        std::slice::from_ref(artifact),
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    )
}

#[test]
fn val_unit_pin_admits_known_codes_and_refuses_unresolved_or_wrong_pins() {
    let base = [units::pack().expect("pack")];
    for (code, _, _, _, _) in units::UNITS {
        let mut artifact = declaration(unit_type(code, norn_ir::UnitSystem::Ucum));
        assert!(unit_findings(&artifact, &base).is_empty());
        assert!(
            unit_findings(&artifact, &[])
                .iter()
                .any(|d| d.code == MISMATCH)
        );
        artifact.dependencies.clear();
        assert!(
            unit_findings(&artifact, &base)
                .iter()
                .any(|d| d.code == MISMATCH)
        );
    }
    for variant in 0..5 {
        let mut artifact = declaration(unit_type("g", norn_ir::UnitSystem::Ucum));
        let pin = artifact.dependencies.first_mut().expect("pin");
        let duplicate = pin.clone();
        match variant {
            0 => pin.version = Some("2.0.0".to_owned()),
            1 => pin.required = false,
            2 => pin.kind = norn_ir::ArtifactKind::Rule,
            3 => artifact.dependencies.push(duplicate),
            _ => {
                let mut hidden = base.clone();
                hidden.first_mut().expect("catalog").scope.org_id = Some("other".to_owned());
                let findings = unit_findings(&artifact, &hidden);
                let refused = findings.iter().any(|d| d.code == MISMATCH);
                assert!(refused);
                continue;
            }
        }
        assert!(
            unit_findings(&artifact, &base)
                .iter()
                .any(|d| d.code == MISMATCH)
        );
    }
}

#[test]
fn val_unit_link_properties_refuse_unknown_units() {
    for (source, root) in [
        (
            include_str!("../../../fixtures/ir/entity-v2/artifact-set/retail-sku.entity.json"),
            "objectType",
        ),
        (
            include_str!(
                "../../../fixtures/ir/entity-v2/artifact-set/acme-sku.extension.entity.json"
            ),
            "objectTypeExtension",
        ),
    ] {
        let mut artifact: serde_json::Value = serde_json::from_str(source).expect("fixture");
        artifact["payload"][root]["links"] = serde_json::json!([{"id":"unit_link","target":"retail.Sku","cardinality":{"to":"many","from":"many"},"properties":[{"id":"amount","type":unit_type("unknown",norn_ir::UnitSystem::Ucum)}]}]);
        let artifact = serde_json::from_value(artifact).expect("typed envelope");
        let pointer = format!("/payload/{root}/links/0/properties/0/type/unit");
        assert!(
            unit_findings(&artifact, &[]).iter().any(
                |d| d.code == MISMATCH && d.span.as_ref().is_some_and(|s| s.pointer == pointer)
            )
        );
    }
}
