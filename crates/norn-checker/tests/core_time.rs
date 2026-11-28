#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "registry correspondence tests"
)]

#[path = "../../../registry/core/src/time.rs"]
mod time;

use std::collections::BTreeSet;

use norn_checker::{ArtifactValidation, entity_instance::EffectiveProperty};
use norn_ir::{
    ArtifactPayload, Property, PropertyConstraints, PropertyId, PropertyType, PropertyTypeDef,
    PropertyTypeName, RuntimeValue,
};

fn catalog() -> Vec<PropertyTypeDef> {
    let pack = time::pack().expect("typed core.time pack");
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
            id: PropertyId("value".to_owned()),
            description: None,
            property_type: PropertyType::Ref {
                name: PropertyTypeName(format!("core.time.{name}")),
                additivity: norn_ir::PropertyAdditivity::Undeclared {},
                unit_source: Vec::new(),
            },
            constraints: PropertyConstraints::default(),
            default: None,
            markings: Vec::new(),
            computed: None,
            protected: false,
            origin: None,
            immutable_after_create: false,
        },
        &catalog(),
    )
}

#[test]
fn core_time_is_the_exact_generated_admitted_catalog() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../registry/core/artifacts/core.time.json");
    let exact =
        RuntimeValue::from_json(serde_json::to_value(time::pack().expect("pack")).expect("wire"))
            .expect("exact runtime value");
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&exact).expect("serializes")
    );
    if std::env::var_os("NORN_CHECKER_UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, &rendered).expect("regenerate");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("generated catalog"),
        rendered
    );
    assert_eq!(
        norn_checker::check_property_type_defs(&catalog(), &BTreeSet::new()),
        []
    );
}

#[test]
fn core_time_declares_all_seven_value_types_and_validates_scalars() {
    let names: BTreeSet<String> = catalog().into_iter().map(|def| def.name.0).collect();
    let expected: BTreeSet<String> = [
        "core.time.instant",
        "core.time.date",
        "core.time.duration",
        "core.time.iana_timezone",
        "core.time.dst_policy",
        "core.time.proper_interval",
        "core.time.date_interval",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect();
    assert_eq!(names, expected);

    let instant = property("instant");
    assert!(
        instant
            .check_runtime_value(&RuntimeValue::from("2025-03-09T14:00:00Z"))
            .is_ok()
    );
    assert!(
        instant
            .check_runtime_value(&RuntimeValue::from("2025-03-09"))
            .is_err()
    );

    let date = property("date");
    assert!(
        date.check_runtime_value(&RuntimeValue::from("2025-03-09"))
            .is_ok()
    );
    assert!(
        date.check_runtime_value(&RuntimeValue::from("2025-03-09T00:00:00Z"))
            .is_err()
    );

    let tz = property("iana_timezone");
    for valid in [
        "UTC",
        "America/New_York",
        "Europe/London",
        "America/Argentina/Buenos_Aires",
        "Etc/GMT+5",
    ] {
        assert!(
            tz.check_runtime_value(&RuntimeValue::from(valid)).is_ok(),
            "valid IANA zone {valid} rejected"
        );
    }
    for invalid in ["", "America/New York", "/UTC", "UTC/"] {
        assert!(
            tz.check_runtime_value(&RuntimeValue::from(invalid))
                .is_err(),
            "invalid IANA zone {invalid} accepted"
        );
    }

    let policy = property("dst_policy");
    for valid in time::DST_POLICIES {
        assert!(
            policy
                .check_runtime_value(&RuntimeValue::from(valid))
                .is_ok()
        );
    }
    for invalid in ["shift", "compatible", "REJECT", ""] {
        assert!(
            policy
                .check_runtime_value(&RuntimeValue::from(invalid))
                .is_err()
        );
    }
}

#[test]
fn core_time_validates_proper_interval_and_date_interval_records() {
    let interval = property("proper_interval");
    assert!(
        interval
            .check_runtime_value(&norn_ir::runtime_object! {
                "starts_at" => "2025-03-09T14:00:00Z",
                "ends_at" => "2025-03-09T22:00:00Z"
            })
            .is_ok()
    );
    assert!(
        interval
            .check_runtime_value(&norn_ir::runtime_object! {
                "starts_at" => "2025-03-09",
                "ends_at" => "2025-03-10"
            })
            .is_err()
    );

    let date_interval = property("date_interval");
    assert!(
        date_interval
            .check_runtime_value(&norn_ir::runtime_object! {
                "starts_at" => "2025-03-09",
                "ends_at" => "2025-03-10"
            })
            .is_ok()
    );
    assert!(
        date_interval
            .check_runtime_value(&norn_ir::runtime_object! {
                "starts_at" => "2025-03-09T00:00:00Z",
                "ends_at" => "2025-03-10T00:00:00Z"
            })
            .is_err()
    );
}
