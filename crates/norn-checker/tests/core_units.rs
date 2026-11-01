#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "registry correspondence tests"
)]

#[path = "../../../registry/core/src/units.rs"]
mod units;

use norn_checker::{ArtifactValidation, entity_instance::EffectiveProperty};
use norn_ir::{
    ArtifactPayload, Property, PropertyConstraints, PropertyId, PropertyType, PropertyTypeDef,
    PropertyTypeName, RuntimeValue,
};
use proptest::prelude::*;

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
    assert!(norn_checker::check_property_type_defs(&catalog(), &Default::default()).is_empty());
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
    }
}
