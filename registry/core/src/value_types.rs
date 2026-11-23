//! Norn-authored core and standard value types (`core.value_types.entity`,
//! `std.value_types.entity`, #1583).

use norn_ir::{
    ArtifactEnvelope, ArtifactLifecycle, ArtifactPayload, ArtifactProvenance, EntityIrVersion,
    EnumValue, MarkingId, PropertyAdditivity, PropertyConstraints, PropertyType,
    PropertyTypeCatalogRootV2, PropertyTypeDef, PropertyTypeName, ProvenanceSource, ScopeRef,
    TextFormat, TimezonePolicy, WholeNumber,
};

/// Active ISO 4217 currency codes: SIX list one (published 2026-09-17),
/// excluding `XTS` (testing) and `XXX` (no currency). Matches `ISO_4217` in
/// `registry/packs/canon/records.ts`.
pub(super) const ISO_4217: [&str; 176] = [
    "AED", "AFN", "ALL", "AMD", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT", "BHD",
    "BIF", "BMD", "BND", "BOB", "BOV", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD", "CDF",
    "CHE", "CHF", "CHW", "CLF", "CLP", "CNY", "COP", "COU", "CRC", "CUP", "CVE", "CZK", "DJF",
    "DKK", "DOP", "DZD", "EGP", "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP",
    "GMD", "GNF", "GTQ", "GYD", "HKD", "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR",
    "ISK", "JMD", "JOD", "JPY", "KES", "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT",
    "LAK", "LBP", "LKR", "LRD", "LSL", "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP",
    "MRU", "MUR", "MVR", "MWK", "MXN", "MXV", "MYR", "MZN", "NAD", "NGN", "NIO", "NOK", "NPR",
    "NZD", "OMR", "PAB", "PEN", "PGK", "PHP", "PKR", "PLN", "PYG", "QAR", "RON", "RSD", "RUB",
    "RWF", "SAR", "SBD", "SCR", "SDG", "SEK", "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN",
    "SVC", "SYP", "SZL", "THB", "TJS", "TMT", "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH",
    "UGX", "USD", "USN", "UYI", "UYU", "UYW", "UZS", "VED", "VES", "VND", "VUV", "WST", "XAD",
    "XAF", "XAG", "XAU", "XBA", "XBB", "XBC", "XBD", "XCD", "XCG", "XDR", "XOF", "XPD", "XPF",
    "XPT", "XSU", "XUA", "YER", "ZAR", "ZMW", "ZWG",
];

fn definition(
    name: &str,
    description: &str,
    property_type: PropertyType,
    constraints: PropertyConstraints,
    markings: &[&str],
) -> PropertyTypeDef {
    PropertyTypeDef {
        name: PropertyTypeName(name.to_owned()),
        description: Some(description.to_owned()),
        property_type,
        constraints,
        markings: markings
            .iter()
            .map(|marking| MarkingId((*marking).to_owned()))
            .collect(),
    }
}

fn plain_decimal(scale: u64) -> PropertyType {
    PropertyType::Decimal {
        additivity: PropertyAdditivity::Undeclared {},
        scale: WholeNumber(scale),
        rounding: None,
        unit: None,
        currency_source: None,
        per_unit: None,
        unit_source: Vec::new(),
    }
}

fn catalog_envelope(
    artifact_id: &str,
    namespace: &str,
    name: &str,
    version: &str,
    property_types: Vec<PropertyTypeDef>,
) -> Result<ArtifactEnvelope, serde_json::Error> {
    let root = PropertyTypeCatalogRootV2 {
        entity_version: EntityIrVersion::V2_0,
        property_types,
        extensions: None,
    };
    Ok(ArtifactEnvelope {
        ir_version: "0.1".to_owned(),
        artifact_id: artifact_id.to_owned(),
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        version: version.to_owned(),
        lifecycle: ArtifactLifecycle::Active,
        scope: ScopeRef::default(),
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::System,
            source_path: Some("registry/core/src/value_types.rs".to_owned()),
            actor_id: None,
            created_at: None,
        },
        payload: ArtifactPayload::Entity(serde_json::from_str(&serde_json::to_string(&root)?)?),
    })
}

fn text_type(
    name: &str,
    description: &str,
    format: Option<TextFormat>,
    min_length: Option<u64>,
    max_length: Option<u64>,
    pattern: Option<&str>,
    markings: &[&str],
) -> PropertyTypeDef {
    definition(
        name,
        description,
        PropertyType::Text { format },
        PropertyConstraints {
            min_length: min_length.map(WholeNumber),
            max_length: max_length.map(WholeNumber),
            pattern: pattern.map(ToOwned::to_owned),
            ..PropertyConstraints::default()
        },
        markings,
    )
}

/// Norn Core shared value types (`core.value_types.entity`).
pub(super) fn core_pack() -> Result<ArtifactEnvelope, serde_json::Error> {
    let property_types = vec![
        text_type(
            "core.display_name",
            "A human-readable name of a party, item, or place.",
            None,
            Some(1),
            Some(200),
            None,
            &[],
        ),
        text_type(
            "core.code",
            "A business code or key assigned by the source system (customer code, item code).",
            None,
            Some(1),
            Some(140),
            None,
            &[],
        ),
        text_type(
            "core.document_number",
            "The number a business document carries (order, invoice, payment reference).",
            None,
            Some(1),
            Some(140),
            None,
            &[],
        ),
        text_type(
            "core.email_address",
            "An email address (RFC 5322 addr-spec).",
            Some(TextFormat::Email),
            None,
            Some(254),
            None,
            &["pii.contact"],
        ),
        text_type(
            "core.phone_number",
            "A telephone number as written by its source; E.164 when normalized.",
            None,
            Some(1),
            Some(50),
            None,
            &["pii.contact"],
        ),
        definition(
            "core.money_amount",
            "A monetary amount. Money is a pair: an amount of this type and an iso4217.currency_code beside it.",
            plain_decimal(6),
            PropertyConstraints::default(),
            &[],
        ),
        definition(
            "core.quantity",
            "A quantity of an item, in the unit recorded beside it.",
            plain_decimal(6),
            PropertyConstraints::default(),
            &[],
        ),
        text_type(
            "core.national_id",
            "A government-issued personal identifier, as reported by its source.",
            None,
            Some(1),
            Some(140),
            None,
            &["pii.identity"],
        ),
        text_type(
            "core.bank_account_number",
            "A bank account identifier, as reported by its source.",
            None,
            Some(1),
            Some(140),
            None,
            &["pii.financial", "confidential.finance"],
        ),
        definition(
            "core.health_information",
            "A health-related record or finding represented as source JSON.",
            PropertyType::JsonExt { schema: None },
            PropertyConstraints::default(),
            &["pii.health"],
        ),
    ];
    catalog_envelope(
        "core.value_types.entity",
        "core",
        "Norn Core value types",
        "0.2.0",
        property_types,
    )
}

fn std_temporal_and_currency_types() -> Vec<PropertyTypeDef> {
    vec![
        definition(
            "iso8601.date",
            "An ISO 8601 calendar date (YYYY-MM-DD), no time and no zone.",
            PropertyType::Date {},
            PropertyConstraints::default(),
            &[],
        ),
        definition(
            "iso8601.timestamp",
            "An ISO 8601 / RFC 3339 instant normalized to UTC.",
            PropertyType::Timestamp {
                timezone: TimezonePolicy::Utc,
            },
            PropertyConstraints::default(),
            &[],
        ),
        definition(
            "iso8601.duration",
            "An exact elapsed duration (ISO 8601 duration with fixed-length units).",
            PropertyType::Duration {},
            PropertyConstraints::default(),
            &[],
        ),
        definition(
            "iso4217.currency_code",
            "An ISO 4217 active currency code (SIX list one, published 2026-09-17, excluding XTS and XXX).",
            PropertyType::Enum {
                values: ISO_4217
                    .into_iter()
                    .map(|value| EnumValue {
                        value: value.to_owned(),
                        description: None,
                    })
                    .collect(),
            },
            PropertyConstraints::default(),
            &[],
        ),
    ]
}

fn std_identifier_types() -> Vec<PropertyTypeDef> {
    vec![
        text_type(
            "ucum.unit_code",
            "A unit of measure in the Unified Code for Units of Measure, case-sensitive form (kg, m/s, {each}).",
            None,
            Some(1),
            Some(64),
            Some("^[!-~]+$"),
            &[],
        ),
        text_type(
            "qudt.unit_id",
            "A QUDT unit, as its `unit:` local name (unit:KiloGM).",
            None,
            None,
            Some(128),
            Some("^unit:[A-Za-z0-9_-]+$"),
            &[],
        ),
        text_type(
            "gs1.gtin",
            "A GS1 Global Trade Item Number: GTIN-8, -12, -13, or -14, digits only (check digit not verified here).",
            None,
            None,
            None,
            Some("^([0-9]{8}|[0-9]{12,14})$"),
            &[],
        ),
        text_type(
            "gs1.gln",
            "A GS1 Global Location Number: 13 digits (check digit not verified here).",
            None,
            None,
            None,
            Some("^[0-9]{13}$"),
            &[],
        ),
        text_type(
            "lei.lei",
            "An ISO 17442 Legal Entity Identifier: 18 alphanumerics and 2 check digits (checksum not verified here).",
            None,
            None,
            None,
            Some("^[A-Z0-9]{18}[0-9]{2}$"),
            &[],
        ),
        text_type(
            "unlocode.location_code",
            "A UN/LOCODE: ISO 3166-1 alpha-2 country code and a 3-character location code.",
            None,
            None,
            None,
            Some("^[A-Z]{2}[A-Z2-9]{3}$"),
            &[],
        ),
        text_type(
            "isco08.occupation_code",
            "An ISCO-08 occupation group code (major to unit group, 1 to 4 digits).",
            None,
            None,
            None,
            Some("^[0-9]{1,4}$"),
            &[],
        ),
        text_type(
            "esco.occupation_uri",
            "An ESCO occupation concept URI.",
            Some(TextFormat::Uri),
            None,
            None,
            Some("^http://data\\.europa\\.eu/esco/occupation/[0-9a-f-]+$"),
            &[],
        ),
    ]
}

/// Standard identifier and code value types (`std.value_types.entity`).
pub(super) fn std_pack() -> Result<ArtifactEnvelope, serde_json::Error> {
    let mut property_types = std_temporal_and_currency_types();
    property_types.extend(std_identifier_types());
    catalog_envelope(
        "std.value_types.entity",
        "std",
        "Standard identifier value types",
        "0.1.0",
        property_types,
    )
}
