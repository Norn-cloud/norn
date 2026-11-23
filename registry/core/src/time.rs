//! Norn-authored core temporal value types (`core.time`, #1584).

use norn_ir::{
    ArtifactEnvelope, ArtifactLifecycle, ArtifactPayload, ArtifactProvenance, EntityIrVersion,
    EnumValue, PropertyConstraints, PropertyId, PropertyType, PropertyTypeCatalogRootV2,
    PropertyTypeDef, PropertyTypeName, ProvenanceSource, RecordMember, ScopeRef, TimezonePolicy,
    WholeNumber,
};

/// Explicit DST fall-back disambiguation policies (`TIME-DST`).
/// Nonexistent spring-forward gaps are always refused regardless of policy.
pub(super) const DST_POLICIES: [&str; 3] = ["reject", "earlier", "later"];

/// Lexical pattern covering all 597 canonical IANA tzdb zone identifiers.
/// Semantic membership against pinned IANA tzdb `2025b` is enforced by
/// `norn_expr::temporal::validate_timezone` and `resolve_local_timestamp`.
pub(super) const IANA_TIMEZONE_PATTERN: &str = "^[A-Za-z0-9_+-]+(/[A-Za-z0-9_+-]+)*$";

fn definition(
    name: &str,
    description: &str,
    property_type: PropertyType,
    constraints: PropertyConstraints,
) -> PropertyTypeDef {
    PropertyTypeDef {
        name: PropertyTypeName(format!("core.time.{name}")),
        description: Some(description.to_owned()),
        property_type,
        constraints,
        markings: Vec::new(),
    }
}

fn member(id: &str, property_type: PropertyType) -> RecordMember {
    RecordMember {
        id: PropertyId(id.to_owned()),
        property_type,
        constraints: PropertyConstraints::default(),
    }
}

fn scalar_types() -> Vec<PropertyTypeDef> {
    vec![
        definition(
            "instant",
            "An exact UTC instant (RFC 3339 normalized to UTC, at most nanosecond precision).",
            PropertyType::Timestamp {
                timezone: TimezonePolicy::Utc,
            },
            PropertyConstraints::default(),
        ),
        definition(
            "date",
            "A proleptic-Gregorian calendar date (YYYY-MM-DD), with no time-of-day and no timezone.",
            PropertyType::Date {},
            PropertyConstraints::default(),
        ),
        definition(
            "duration",
            "An exact elapsed duration (ISO 8601 duration with fixed-length units).",
            PropertyType::Duration {},
            PropertyConstraints::default(),
        ),
        definition(
            "iana_timezone",
            "A canonical IANA timezone identifier from the pinned timezone database (tzdb 2025b).",
            PropertyType::Text { format: None },
            PropertyConstraints {
                min_length: Some(WholeNumber(1)),
                max_length: Some(WholeNumber(64)),
                pattern: Some(IANA_TIMEZONE_PATTERN.to_owned()),
                ..PropertyConstraints::default()
            },
        ),
        definition(
            "dst_policy",
            "Explicit disambiguation policy for ambiguous DST fall-back wall times (reject, earlier, or later).",
            PropertyType::Enum {
                values: DST_POLICIES
                    .into_iter()
                    .map(|value| EnumValue {
                        value: value.to_owned(),
                        description: None,
                    })
                    .collect(),
            },
            PropertyConstraints::default(),
        ),
    ]
}

fn interval_types() -> Vec<PropertyTypeDef> {
    vec![
        definition(
            "proper_interval",
            "A half-open UTC instant interval [starts_at, ends_at) where starts_at < ends_at.",
            PropertyType::Record {
                members: vec![
                    member(
                        "starts_at",
                        PropertyType::Timestamp {
                            timezone: TimezonePolicy::Utc,
                        },
                    ),
                    member(
                        "ends_at",
                        PropertyType::Timestamp {
                            timezone: TimezonePolicy::Utc,
                        },
                    ),
                ],
            },
            PropertyConstraints::default(),
        ),
        definition(
            "date_interval",
            "A calendar date interval bounded by starts_at and ends_at.",
            PropertyType::Record {
                members: vec![
                    member("starts_at", PropertyType::Date {}),
                    member("ends_at", PropertyType::Date {}),
                ],
            },
            PropertyConstraints::default(),
        ),
    ]
}

/// Core temporal value types: instant, date, duration, IANA timezone, DST
/// policy, proper timestamp interval, and calendar date interval.
pub(super) fn pack() -> Result<ArtifactEnvelope, serde_json::Error> {
    let mut property_types = scalar_types();
    property_types.extend(interval_types());
    let root = PropertyTypeCatalogRootV2 {
        entity_version: EntityIrVersion::V2_0,
        property_types,
        extensions: None,
    };
    Ok(ArtifactEnvelope {
        ir_version: "0.1".to_owned(),
        artifact_id: "core.time".to_owned(),
        namespace: "core".to_owned(),
        name: "Core temporal value types".to_owned(),
        version: "1.0.0".to_owned(),
        lifecycle: ArtifactLifecycle::Active,
        scope: ScopeRef::default(),
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::System,
            source_path: Some("registry/core/src/time.rs".to_owned()),
            actor_id: None,
            created_at: None,
        },
        payload: ArtifactPayload::Entity(serde_json::from_str(&serde_json::to_string(&root)?)?),
    })
}
