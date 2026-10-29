#![allow(
    clippy::expect_used,
    clippy::pedantic,
    missing_docs,
    reason = "focused contract tests use fixture-oriented helpers"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

use norn_checker::ArtifactValidation;
use norn_checker::validate_ontology_instance;
use norn_ir::{ArtifactEnvelope, JsonValue};

const PACK_FIXTURE: &str =
    include_str!("../../../verification/ontology/packs/quantity-units-v0/pack.json");
const OWL_PROJECTION: &str =
    include_str!("../../../verification/ontology/packs/quantity-units-v0/ontology.owl.ttl");
const SHACL_PROJECTION: &str =
    include_str!("../../../verification/ontology/packs/quantity-units-v0/shapes.shacl.ttl");
const PROJECTION_CONTRACT: &str =
    include_str!("../../../verification/ontology/packs/quantity-units-v0/projection-contract.json");

fn fixture_json() -> serde_json::Value {
    serde_json::from_str(PACK_FIXTURE).expect("quantity-unit fixture is valid JSON")
}

#[expect(
    clippy::unwrap_in_result,
    reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
)]
fn validate_fixture(value: serde_json::Value) -> Result<(), Vec<norn_ir::Diagnostic>> {
    serde_json::from_value::<ArtifactEnvelope>(value)
        .expect("test mutation must remain valid envelope JSON")
        .validate()
}

fn fixture_payload() -> JsonValue {
    let artifact: ArtifactEnvelope =
        serde_json::from_str(PACK_FIXTURE).expect("fixture must decode as ArtifactEnvelope");
    artifact.payload.to_json_value()
}

fn kilogram_instance(symbol: &serde_json::Value, observed_at: &str) -> JsonValue {
    serde_json::from_value(serde_json::json!({
        "instanceId": "unit.kilogram.example",
        "termId": "norn.measure.unit.kilogram",
        "values": {
            "symbol": [symbol],
            "quantityKind": ["norn.measure.quantity-kind.mass"]
        },
        "provenance": {
            "sourceKind": "norn_projection",
            "sourceRef": "norn.ontology-pack.quantity-units",
            "observedAt": observed_at,
            "evidenceRefs": ["https://example.invalid/evidence/unit-kilogram"]
        }
    }))
    .expect("instance must decode as JsonValue")
}

#[test]
fn pinned_quantity_unit_ontology_pack_validates() {
    validate_fixture(fixture_json()).expect("pinned quantity-unit pack should validate");
}

#[test]
fn qudt_is_the_only_mapping_source_and_ucum_stays_reference_only() {
    let fixture = fixture_json();
    let sources = fixture["payload"]["sources"]
        .as_array()
        .expect("sources must be an array");
    let qudt = sources
        .iter()
        .find(|source| source["sourceId"] == "source.qudt")
        .expect("QUDT source pin must exist");
    assert_eq!(
        qudt["releaseId"],
        "v3.4.0@1137205617d03d3d5c8351ea58105b6719c5d6f0"
    );
    assert_eq!(qudt["redistribution"], "reference_only");

    let ucum = sources
        .iter()
        .find(|source| source["sourceId"] == "source.ucum")
        .expect("UCUM reference pin must exist");
    assert_eq!(ucum["releaseId"], "2.2@2024-06-17");
    assert_eq!(ucum["redistribution"], "reference_only");
    assert!(
        ucum.get("files").is_none(),
        "UCUM content must not be vendored"
    );

    let mappings = fixture["payload"]["mappings"]
        .as_array()
        .expect("mappings must be an array");
    assert!(
        mappings
            .iter()
            .all(|mapping| mapping["sourceId"] == "source.qudt")
    );
    assert!(mappings.iter().all(|mapping| matches!(
        mapping["relation"].as_str(),
        Some("close_match" | "related_match")
    )));
    assert!(!PACK_FIXTURE.contains("ucumCode"));
    assert!(!PACK_FIXTURE.contains("conversionMultiplier"));
}

#[test]
fn exact_mapping_requires_ratification_and_a_valid_utc_timestamp() {
    let mut candidate = fixture_json();
    candidate["payload"]["mappings"][0]["relation"] = serde_json::json!("exact_match");
    let diagnostics =
        validate_fixture(candidate.clone()).expect_err("unratified exact mapping must be rejected");
    let paths: Vec<_> = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.path.as_deref())
        .collect();
    assert!(paths.contains(&"payload.mappings[0].ratifiedBy"));
    assert!(paths.contains(&"payload.mappings[0].ratifiedAt"));

    candidate["payload"]["mappings"][0]["ratifiedBy"] = serde_json::json!("principal.reviewer");
    candidate["payload"]["mappings"][0]["ratifiedAt"] = serde_json::json!("2026-99-99T99:99:99Z");
    let diagnostics = validate_fixture(candidate)
        .expect_err("exact mapping with an invalid ratification time must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].ratifiedAt")
            && diagnostic.message.contains("UTC RFC3339")
    }));

    let mut candidate = fixture_json();
    candidate["payload"]["mappings"][0]["relation"] = serde_json::json!("exact_match");
    candidate["payload"]["mappings"][0]["ratifiedBy"] = serde_json::json!("principal.reviewer");
    candidate["payload"]["mappings"][0]["ratifiedAt"] = serde_json::json!("2026-08-23T12:34:56Z");
    validate_fixture(candidate).expect("explicitly ratified exact mapping should validate");
}

#[test]
fn calendar_dates_and_https_iris_are_checked_conservatively() {
    for (path, replacement) in [
        ("releasedAt", serde_json::json!("2024-02-30")),
        ("canonicalUrl", serde_json::json!("https://")),
        ("canonicalUrl", serde_json::json!("http://ucum.org/ucum")),
    ] {
        let mut candidate = fixture_json();
        candidate["payload"]["sources"][1][path] = replacement;
        let diagnostics =
            validate_fixture(candidate).expect_err("invalid source metadata must fail");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some(&format!("payload.sources[1].{path}"))
        }));
    }

    let mut candidate = fixture_json();
    candidate["payload"]["mappings"][0]["externalIri"] =
        serde_json::json!("http://qudt.org/vocab/quantitykind/Count");
    let diagnostics = validate_fixture(candidate).expect_err("plaintext mapping IRI must fail");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].externalIri")
    }));
}

#[test]
fn instance_predicates_use_the_closed_rust_types() {
    let payload = fixture_payload();
    validate_ontology_instance(
        &payload,
        &kilogram_instance(&serde_json::json!("kg"), "2026-08-23T12:34:56.123Z"),
    )
    .expect("valid unit instance and UTC timestamp should pass");

    let diagnostics = validate_ontology_instance(
        &payload,
        &kilogram_instance(&serde_json::json!("kg"), "2026-99-99T99:99:99Z"),
    )
    .expect_err("invalid provenance timestamp must fail");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("instance.provenance.observedAt")
    }));

    for (value_type, valid, invalid) in [
        (
            "date_time",
            serde_json::json!("2024-02-29T23:59:59Z"),
            serde_json::json!("2023-02-29T23:59:59Z"),
        ),
        (
            "iri",
            serde_json::json!("https://example.com/unit/kg"),
            serde_json::json!("https://"),
        ),
        (
            "integer",
            serde_json::json!(42),
            serde_json::json!(9_007_199_254_740_992_u64),
        ),
    ] {
        let mut candidate = fixture_json();
        candidate["payload"]["shapes"][7]["fields"][0]["valueType"] = serde_json::json!(value_type);
        let artifact: ArtifactEnvelope =
            serde_json::from_value(candidate).expect("mutated fixture must decode");
        let payload = artifact.payload.to_json_value();
        validate_ontology_instance(&payload, &kilogram_instance(&valid, "2026-08-23T12:34:56Z"))
            .expect("value in the supported closed subset should pass");
        let diagnostics = validate_ontology_instance(
            &payload,
            &kilogram_instance(&invalid, "2026-08-23T12:34:56Z"),
        )
        .expect_err("value outside the supported closed subset must fail");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.path.as_deref() == Some("instance.values.symbol[0]")
            })
        );
    }
}

#[test]
fn malformed_selected_shape_fails_before_instance_validation() {
    let mut candidate = fixture_json();
    candidate["payload"]["shapes"][7]
        .as_object_mut()
        .expect("shape must be an object")
        .remove("fields");
    let artifact: ArtifactEnvelope =
        serde_json::from_value(candidate).expect("mutated fixture must decode");
    let payload = artifact.payload.to_json_value();
    let diagnostics = validate_ontology_instance(
        &payload,
        &kilogram_instance(&serde_json::json!("kg"), "2026-08-23T12:34:56Z"),
    )
    .expect_err("malformed selected shape must fail closed");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.path.as_deref() == Some("payload.shapes[7].fields") })
    );
}

#[test]
fn conversion_and_transition_authority_cannot_be_smuggled() {
    for forbidden_field in [
        "conversionMultiplier",
        "ucumCode",
        "capabilities",
        "effects",
        "transitionGuards",
    ] {
        let mut candidate = fixture_json();
        candidate["payload"]["terms"][7][forbidden_field] = serde_json::json!("forbidden");
        let diagnostics = validate_fixture(candidate)
            .expect_err("closed ontology term must reject authority or conversion fields");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some(&format!("payload.terms[7].{forbidden_field}"))
        }));
    }
}

#[test]
fn owl_shacl_and_projection_contract_are_non_authoritative() {
    assert!(OWL_PROJECTION.contains("owl:versionIRI"));
    assert!(OWL_PROJECTION.contains("skos:closeMatch"));
    assert!(!OWL_PROJECTION.contains("owl:imports"));
    assert!(!OWL_PROJECTION.contains("conversionMultiplier"));
    assert!(!OWL_PROJECTION.contains("ucumCode"));
    assert!(!OWL_PROJECTION.contains("authorizesTransition"));
    assert!(!OWL_PROJECTION.contains("grantsCapability"));

    assert!(SHACL_PROJECTION.contains("sh:closed true"));
    assert!(SHACL_PROJECTION.contains("differential-oracle projection"));
    assert!(!SHACL_PROJECTION.contains("conversionMultiplier"));
    assert!(!SHACL_PROJECTION.contains("authorizesTransition"));

    let contract: serde_json::Value =
        serde_json::from_str(PROJECTION_CONTRACT).expect("projection contract must be valid JSON");
    assert_eq!(contract["shaclProjection"]["promotionAuthority"], false);
    assert_eq!(
        contract["quantityBoundary"]["conversionRulesDefined"],
        false
    );
    assert_eq!(
        contract["quantityBoundary"]["conversionExecutionAllowed"],
        false
    );
    assert_eq!(contract["quantityBoundary"]["ucumContentVendored"], false);
    assert_eq!(contract["quantityBoundary"]["ucumMappingsDefined"], false);
}
