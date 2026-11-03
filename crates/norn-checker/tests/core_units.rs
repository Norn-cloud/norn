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
        additivity: norn_ir::PropertyAdditivity::Undeclared,

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

#[test]
fn val_unit_literal_note_is_retained_without_refusing_a_valid_artifact() {
    let mut artifact: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/rule-ir-v2/valid/reorder-priority.rule.json"
    ))
    .expect("sub-decision fixture");
    artifact["dependencies"] = serde_json::json!([{"artifactId":"core.units","kind":"entity","version":"1.0.0","required":true}]);
    artifact["payload"]["inputSchema"] = serde_json::json!({"type":"object","properties":{"mass":{"type":"number","nornType":unit_type("g",norn_ir::UnitSystem::Ucum)}},"required":["mass"],"additionalProperties":false});
    artifact["payload"]["outputSchema"] = serde_json::json!({"type":"object","properties":{"positive":{"type":"boolean"}},"required":["positive"],"additionalProperties":false});
    artifact["payload"]
        .as_object_mut()
        .expect("payload")
        .remove("defaults");
    artifact["payload"]["graph"] = serde_json::json!({"nodes":[
        {"id":"input","name":"input","type":"inputNode"},
        {"id":"decision","name":"decision","type":"expressionNode","content":{"expressions":[{"id":"positive","key":"positive","value":"input.mass > 0"}]}},
        {"id":"output","name":"output","type":"outputNode"}
    ],"edges":[{"id":"in","sourceId":"input","targetId":"decision"},{"id":"out","sourceId":"decision","targetId":"output"}]});
    let artifact: norn_ir::ArtifactEnvelope =
        serde_json::from_value(artifact).expect("typed artifact");
    let findings = norn_checker::check_artifact(&artifact);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "core.values.literal_unit"
                && finding.severity == norn_ir::DiagnosticSeverity::Info)
    );
    artifact
        .validate()
        .expect("informational unit inference does not refuse validation");
    assert!(
        unit_findings(&artifact, &[units::pack().expect("pack")])
            .iter()
            .all(|finding| finding.severity != norn_ir::DiagnosticSeverity::Error)
    );
}

#[test]
fn val_unit_unrelated_deep_payload_does_not_report_a_unit_mismatch() {
    let mut artifact = serde_json::to_value(declaration(unit_type("g", norn_ir::UnitSystem::Ucum)))
        .expect("artifact");
    let mut nested = serde_json::Value::Null;
    for _ in 0..80 {
        nested = serde_json::json!([nested]);
    }
    artifact["payload"]["extensions"] = serde_json::json!({"test.metadata/1.0.0":nested});
    let artifact = serde_json::from_value(artifact).expect("typed artifact");
    assert!(
        !unit_findings(&artifact, &[units::pack().expect("pack")])
            .iter()
            .any(|finding| finding.code == MISMATCH)
    );
}

proptest! {
    #[test]
    fn val_unit_action_literal_thresholds_keep_the_inferred_unit(minimum in 0_u16..1000) {
        let mut document: serde_json::Value = serde_json::from_str(include_str!("../../../tools/norn-ts/fixtures/definitions/jewelry-pricing.ir.json")).expect("compiled fixture");
        let action = document.as_array_mut().expect("artifact set").iter_mut().find(|a| a["kind"] == "action").expect("action");
        action["payload"]["preconditions"][0] = format!("fine_g > {minimum}").into();
        let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_value(document).expect("threshold variation");
        let diagnostics = norn_checker::check_artifact_set(&artifacts, &norn_checker::EntityArtifactSetContext { registry: &BTreeSet::new(), base: &[units::pack().expect("units")], tenant: None });
        prop_assert!(diagnostics.iter().all(|d| d.severity != norn_ir::DiagnosticSeverity::Error), "{diagnostics:?}");
        prop_assert!(diagnostics.iter().any(|d| d.code == "core.values.literal_unit" && d.message.contains("unit g") && d.severity == norn_ir::DiagnosticSeverity::Info), "{diagnostics:?}");
    }
}

#[test]
fn val_unit_action_info_cannot_refuse_or_hide_an_error() {
    for invalid in [false, true] {
        let mut document: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tools/norn-ts/fixtures/definitions/jewelry-pricing.ir.json"
        ))
        .expect("compiled pricing declarations");
        if invalid {
            let action = document
                .as_array_mut()
                .expect("artifact set")
                .iter_mut()
                .find(|artifact| artifact["kind"] == "action")
                .expect("action");
            action["payload"]["preconditions"]
                .as_array_mut()
                .expect("preconditions")
                .push("input.missing > 0".into());
        }
        let artifacts: Vec<norn_ir::ArtifactEnvelope> =
            serde_json::from_value(document).expect("declarations");
        let action = artifacts
            .iter()
            .find(|artifact| artifact.kind() == norn_ir::ArtifactKind::Action)
            .expect("action");
        let local = action.validate();
        assert_eq!(
            local.is_err(),
            invalid,
            "Info alone never refuses local admission"
        );
        if let Err(diagnostics) = local {
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.severity == norn_ir::DiagnosticSeverity::Info)
            );
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.severity == norn_ir::DiagnosticSeverity::Error)
            );
        }
        let base = [units::pack().expect("units")];
        let candidates = norn_ir::runtime::encode(&artifacts).expect("exact sources");
        let base_source = norn_ir::runtime::encode(&base).expect("exact base");
        let sources = norn_checker::ReceiptSources::new(
            candidates.as_array().expect("source array"),
            base_source.as_array().expect("base array"),
        )
        .expect("receipt sources");
        let checked = norn_checker::check_artifact_set_with_receipt(
            &sources,
            &norn_checker::EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None,
            },
            &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("receipt check");
        assert_eq!(checked.receipt.admitted, !invalid);
        assert_eq!(
            checked
                .diagnostics
                .iter()
                .any(norn_checker::Diagnostic::is_error),
            invalid
        );
        assert_eq!(checked.receipt.type_env.is_none(), invalid);
        assert!(
            checked
                .diagnostics
                .iter()
                .any(|d| d.code == "core.values.literal_unit"
                    && d.severity == norn_ir::DiagnosticSeverity::Info)
        );
    }
}
