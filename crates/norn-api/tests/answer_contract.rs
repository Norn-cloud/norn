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
        assert!(
            errors.is_empty(),
            "{file} does not satisfy {root}: {errors:#?}"
        );
    }
}

/// Every engine the cell can answer with is one the contract names: an
/// answer from `DuckDB` validates as a `NornQueryResult`, so an enum-checking
/// client (the generated Python and TypeScript SDKs) accepts it. The match
/// makes a new runtime engine a compile error here until the contract names it.
#[test]
fn every_runtime_engine_validates_against_the_contract() {
    use norn_cell::compute::Engine;
    // An exhaustive match: a new engine is a compile error until it is listed.
    fn listed(engine: Engine) -> Engine {
        match engine {
            Engine::Ivm => Engine::Ivm,
            Engine::Cell => Engine::Cell,
            Engine::Duckdb => Engine::Duckdb,
        }
    }
    let engines = || [Engine::Ivm, Engine::Cell, Engine::Duckdb];
    let contract: Value =
        serde_json::from_str(&norn_api::openapi::openapi_json().expect("contract"))
            .expect("contract JSON");
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "allOf": [{ "$ref": "#/components/schemas/NornQueryResult" }],
        "components": contract["components"],
    });
    let validator = jsonschema::validator_for(&schema).expect("contract schema compiles");
    let directory = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/demo/answers");
    let answer: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{directory}/scrap-yield.k1.answer.json"))
            .expect("retained answer"),
    )
    .expect("answer JSON");
    for engine in engines() {
        let mut document = answer.clone();
        document["engine"] = serde_json::to_value(listed(engine)).expect("engine");
        let errors: Vec<String> = validator
            .iter_errors(&document)
            .map(|error| error.to_string())
            .collect();
        assert!(errors.is_empty(), "{engine:?}: {errors:#?}");
    }
}
