//! Retained engine answers (`fixtures/demo/answers`) satisfy the generated contract.
#![allow(
    clippy::expect_used,
    reason = "contract conformance over retained documents"
)]
use serde_json::{Value, json};

#[test]
fn retained_answer_documents_validate_against_their_projected_schemas() {
    let contract: Value =
        serde_json::from_str(&norn_api::openapi::openapi_json().expect("contract"))
            .expect("contract JSON");
    let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/demo/answers");
    for (file, root) in [
        ("scrap-yield.question.json", "NornIrQuestion"),
        (
            "scrap-yield.quarantine.finding.json",
            "NornIrDataQualityFinding",
        ),
        ("scrap-yield.object-type.json", "NornIrObjectTypeDef"),
        ("scrap-yield.metrics.json", "NornIrMetricDefinition"),
        ("scrap-yield.k1.answer.json", "NornQueryResult"),
        ("scrap-yield.k1-quarantined.answer.json", "NornQueryResult"),
        ("scrap-yield.k2.answer.json", "NornQueryResult"),
        (
            "scrap-yield.k1-as-of.answer.json",
            "NornHistoricalQuestionResult",
        ),
    ] {
        let reference = json!({ "$ref": format!("#/components/schemas/{root}") });
        let root = if file.ends_with(".metrics.json") {
            json!({ "type": "array", "items": reference })
        } else {
            reference
        };
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "allOf": [root],
            "components": contract["components"],
        });
        let validator = jsonschema::validator_for(&schema).expect("contract schema compiles");
        let document: Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{directory}/{file}")).expect("retained document"),
        )
        .expect("document JSON");
        let errors: Vec<String> = validator
            .iter_errors(&document)
            .map(|error| error.to_string())
            .collect();
        assert_eq!(
            errors.len(),
            0,
            "{file} does not satisfy {root}: {errors:#?}"
        );
    }
}
