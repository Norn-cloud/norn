#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "focused contract tests use fixture-oriented helpers"
)]

use norn_checker::ArtifactValidation;
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

fn link() -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": "norn.link.fixture-shop.fixture-canon",
        "kind": "business_pack",
        "namespace": "norn.ontology",
        "name": "Fixture shop to canon",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": {"source": "authoring_sdk", "sourcePath": "fixture.ts"},
        "payload": {
            "packId": "norn.link.fixture-shop.fixture-canon",
            "packVersion": "0.1",
            "packKind": "link",
            "fromPack": {"packId": "norn.ontology-pack.fixture-shop", "version": "0.1.0"},
            "toPack": {"packId": "norn.ontology-pack.fixture-canon", "version": "0.1.0"},
            "correspondences": [
                {
                    "correspondenceId": "corr.order",
                    "fromTermId": "norn.shop.order",
                    "relation": "subsumed_by",
                    "toTermIds": ["norn.canon.commitment"],
                    "justification": "A shop order is a commitment to transfer goods for payment.",
                    "evidenceRefs": ["source.shop-api", "https://example.com/docs/order"],
                    "confidenceBasisPoints": 8000,
                    "reviewedBy": "fixture",
                    "reviewedAt": "2026-10-30T00:00:00Z"
                },
                {
                    "correspondenceId": "corr.order.outstanding",
                    "fromTermId": "norn.shop.order.outstanding",
                    "relation": "derived",
                    "toTermIds": ["norn.canon.claim.amount", "norn.canon.settlement.amount"],
                    "justification": "Outstanding is the claim less its settlements.",
                    "evidenceRefs": ["source.shop-api"],
                    "confidenceBasisPoints": 6000,
                    "reviewedBy": "fixture",
                    "reviewedAt": "2026-10-30T00:00:00Z"
                }
            ],
            "residuals": [
                {"termId": "norn.shop.order.theme", "reason": "system_specific", "note": "Storefront presentation."}
            ]
        }
    })
}

#[expect(
    clippy::unwrap_in_result,
    reason = "a test mutation must stay envelope JSON; failing to decode is a test bug, not a refusal"
)]
fn validate(value: Value) -> Result<(), Vec<norn_ir::Diagnostic>> {
    serde_json::from_value::<ArtifactEnvelope>(value)
        .expect("fixture must stay envelope JSON")
        .validate()
}

fn refuses(value: Value, path: &str, message: &str) {
    let diagnostics = validate(value).expect_err("link must be refused");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some(path) && diagnostic.message.contains(message)
        }),
        "expected '{message}' at {path}: {diagnostics:#?}"
    );
}

#[test]
fn a_link_with_subsumed_derived_and_residual_terms_validates() {
    validate(link()).expect("fixture link validates");
}

#[test]
fn link_identity_and_endpoints_are_pinned() {
    let mut candidate = link();
    candidate["payload"]["packId"] = json!("shop.to.canon");
    refuses(candidate, "payload.packId", "norn.link.*");

    let mut candidate = link();
    candidate["payload"]["toPack"] = candidate["payload"]["fromPack"].clone();
    refuses(candidate, "payload.toPack.packId", "two different packs");

    for version in ["latest", "1.2", "v1.2.3", "01.2.3"] {
        let mut candidate = link();
        candidate["payload"]["fromPack"]["version"] = json!(version);
        refuses(
            candidate,
            "payload.fromPack.version",
            "exact semantic version",
        );
    }
    for version in ["1.2.3-rc.1", "1.2.3+build.1"] {
        let mut candidate = link();
        candidate["payload"]["fromPack"]["version"] = json!(version);
        validate(candidate).expect("a strict SemVer pin is exact");
    }
}

#[test]
fn relation_arity_and_transforms_follow_the_relation() {
    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["toTermIds"] =
        json!(["norn.canon.commitment", "norn.canon.claim"]);
    refuses(
        candidate,
        "payload.correspondences[0].toTermIds",
        "only a derived",
    );

    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["transformRef"] = json!("shop.order.mapping");
    refuses(
        candidate,
        "payload.correspondences[0].transformRef",
        "only a derived",
    );

    let mut candidate = link();
    candidate["payload"]["correspondences"][1]["transformRef"] = json!("shop.order.mapping");
    validate(candidate).expect("a derived correspondence may name its transform");

    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["relation"] = json!("close_match");
    refuses(candidate, "payload.correspondences[0].relation", "one of");
}

#[test]
fn correspondences_cite_the_source_manifest_with_bounded_confidence() {
    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["evidenceRefs"] =
        json!(["https://example.com/docs/order"]);
    refuses(
        candidate,
        "payload.correspondences[0].evidenceRefs",
        "at least one source",
    );

    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["evidenceRefs"] =
        json!(["source.shop-api", "notes"]);
    refuses(
        candidate,
        "payload.correspondences[0].evidenceRefs[1]",
        "absolute HTTPS IRI",
    );

    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["confidenceBasisPoints"] = json!(10001);
    refuses(
        candidate,
        "payload.correspondences[0].confidenceBasisPoints",
        "0 through 10000",
    );
}

#[test]
fn residual_reasons_are_closed_and_include_non_conforming_behaviour() {
    let mut candidate = link();
    candidate["payload"]["residuals"][0]["reason"] = json!("non_conforming");
    validate(candidate).expect("non-conforming source behaviour is a residual reason");

    let mut candidate = link();
    candidate["payload"]["residuals"][0]["reason"] = json!("ignored");
    refuses(candidate, "payload.residuals[0].reason", "one of");
}

#[test]
fn a_term_is_either_mapped_or_residual() {
    let mut candidate = link();
    candidate["payload"]["residuals"][0]["termId"] = json!("norn.shop.order");
    refuses(
        candidate,
        "payload.residuals[0].termId",
        "cannot also be residual",
    );

    let mut candidate = link();
    candidate["payload"]["correspondences"][1]["correspondenceId"] = json!("corr.order");
    refuses(
        candidate,
        "payload.correspondences[1].correspondenceId",
        "unique",
    );
}
