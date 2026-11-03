//! Norn-authored bounded unit definitions; no implicit conversion operation.

use norn_ir::{
    ArtifactEnvelope, ArtifactLifecycle, ArtifactPayload, ArtifactProvenance, DecimalLiteral,
    EntityIrVersion, EnumValue, PropertyConstraints, PropertyId, PropertyType,
    PropertyTypeCatalogRootV2, PropertyTypeDef, PropertyTypeName, ProvenanceSource, RecordMember,
    ScopeRef,
};

/// UCUM essence 2.2, exact upstream file; the XML itself is not redistributed.
pub(super) const ESSENCE: &str = "https://raw.githubusercontent.com/ucum-org/ucum/ef214f6da4e060690f1dca21873d535c35cd9793/ucum-essence.xml#version=2.2&sha256=dfccea1b5dc284245ebae97edd1dc03c45864da4e87df55bc9851797b4fd0b61";

/// Code, value-type member, dimension, numerator and denominator to its base.
/// Bases are gram, unity, each and second. Currencies are separate money types.
pub(super) const UNITS: [(&str, &str, &str, i64, i64); 12] = [
    ("g", "g", "mass", 1, 1),
    ("mg", "mg", "mass", 1, 1000),
    ("kg", "kg", "mass", 1000, 1),
    ("1", "one", "dimensionless", 1, 1),
    ("%", "percent", "dimensionless", 1, 100),
    ("[ppth]", "per_mille", "dimensionless", 1, 1000),
    ("{each}", "each", "count", 1, 1),
    ("s", "s", "time", 1, 1),
    ("min", "min", "time", 60, 1),
    ("h", "h", "time", 3600, 1),
    ("d", "d", "time", 86400, 1),
    ("wk", "wk", "time", 604_800, 1),
];

fn enumeration(values: impl IntoIterator<Item = impl Into<String>>) -> PropertyType {
    PropertyType::Enum {
        values: values
            .into_iter()
            .map(|value| EnumValue {
                value: value.into(),
                description: None,
            })
            .collect(),
    }
}

fn member(id: &str, property_type: PropertyType, fixed: Option<i64>) -> RecordMember {
    RecordMember {
        id: PropertyId(id.to_owned()),
        property_type,
        constraints: PropertyConstraints {
            min: fixed.map(|value| DecimalLiteral(value.to_string())),
            max: fixed.map(|value| DecimalLiteral(value.to_string())),
            ..PropertyConstraints::default()
        },
    }
}

fn definition(name: &str, property_type: PropertyType) -> PropertyTypeDef {
    PropertyTypeDef {
        name: PropertyTypeName(format!("core.units.{name}")),
        description: None,
        property_type,
        constraints: PropertyConstraints::default(),
        markings: Vec::new(),
    }
}

/// VAL-UNIT-1: the bounded catalog carries exact, immutable code/factor records.
/// Membership and record constraints are enforced by the ordinary value checker.
pub(super) fn pack() -> Result<ArtifactEnvelope, serde_json::Error> {
    let mut property_types = vec![
        definition("code", enumeration(UNITS.iter().map(|unit| unit.0))),
        definition(
            "dimension",
            enumeration(["mass", "dimensionless", "count", "time"]),
        ),
    ];
    property_types.extend(
        UNITS
            .iter()
            .map(|&(code, name, dimension, numerator, denominator)| {
                definition(
                    name,
                    PropertyType::Record {
                        members: vec![
                            member("code", enumeration([code]), None),
                            member("dimension", enumeration([dimension]), None),
                            member(
                                "numerator",
                                PropertyType::Integer {
                                    additivity: norn_ir::PropertyAdditivity::Undeclared,
                                },
                                Some(numerator),
                            ),
                            member(
                                "denominator",
                                PropertyType::Integer {
                                    additivity: norn_ir::PropertyAdditivity::Undeclared,
                                },
                                Some(denominator),
                            ),
                        ],
                    },
                )
            }),
    );
    let root = PropertyTypeCatalogRootV2 {
        entity_version: EntityIrVersion::V2_0,
        property_types,
        extensions: None,
    };
    Ok(ArtifactEnvelope {
        ir_version: "0.1".to_owned(),
        artifact_id: "core.units".to_owned(),
        namespace: "core".to_owned(),
        name: "Bounded core units".to_owned(),
        version: "1.0.0".to_owned(),
        lifecycle: ArtifactLifecycle::Active,
        scope: ScopeRef::default(),
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::System,
            source_path: Some(ESSENCE.to_owned()),
            actor_id: None,
            created_at: None,
        },
        payload: ArtifactPayload::Entity(serde_json::from_str(&serde_json::to_string(&root)?)?),
    })
}
