#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "focused contract tests use fixture-oriented helpers"
)]

use norn_checker::{ArtifactValidation, validate_link_registry};
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

    let mut candidate = link();
    candidate["payload"]["fromPack"]["version"] = json!("latest");
    refuses(
        candidate,
        "payload.fromPack.version",
        "exact semantic version",
    );
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

fn pack(id: &str, terms: Value, shaped: &[&str]) -> Value {
    let shapes: Vec<Value> = shaped
        .iter()
        .map(|term| {
            json!({"shapeId": format!("shape.{term}"), "termId": term, "closed": true,
                   "fields": [{"fieldId": "id", "valueType": "string", "minCount": 1, "maxCount": 1}]})
        })
        .collect();
    let first = terms[0]["termId"].clone();
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": "business_pack", "namespace": "norn.ontology",
        "name": id, "version": "0.1.0", "lifecycle": "draft", "scope": {}, "dependencies": [],
        "provenance": {"source": "authoring_sdk", "sourcePath": "fixture.ts"},
        "payload": {
            "packId": id, "packVersion": "0.1", "packKind": "ontology",
            "sources": [
                {"sourceId": "source.shop-api", "releaseId": "2026-10@0f0c97a4f666b2f8563573fe48453782fd51b87a504523cf0c9aff6a71c3eec4",
                 "releasedAt": "2026-10-01", "retrievedAt": "2026-10-30", "canonicalUrl": "https://example.com/docs/",
                 "license": "LicenseRef-Example-API-Terms", "redistribution": "reference_only", "importMode": "map_only"},
                {"sourceId": "source.shop-blog", "releaseId": "2026-10@2026-10-01", "releasedAt": "2026-10-01",
                 "retrievedAt": "2026-10-30", "canonicalUrl": "https://example.com/blog/", "license": "LicenseRef-Example",
                 "redistribution": "reference_only", "importMode": "study_only"}
            ],
            "terms": terms,
            "mappings": [{"mappingId": "map.first", "termId": first, "sourceId": "source.shop-api",
                          "externalIri": "https://example.com/docs/first", "relation": "close_match",
                          "justification": "Fixture.", "evidenceRefs": ["https://example.com/docs/first"],
                          "confidenceBasisPoints": 5000, "reviewedBy": "fixture", "reviewedAt": "2026-10-30T00:00:00Z"}],
            "shapes": shapes
        }
    })
}

fn term(id: &str, kind: &str, domain: Option<&str>) -> Value {
    let mut term = json!({"termId": id, "label": id, "definition": "Fixture term.", "termKind": kind,
                          "status": "active", "semanticAuthority": "semantic_only"});
    if let Some(domain) = domain {
        term["domainTermId"] = json!(domain);
    }
    term
}

fn registry(link: Value) -> Vec<ArtifactEnvelope> {
    let shop = pack(
        "norn.ontology-pack.fixture-shop",
        json!([
            term("norn.shop.order", "class", None),
            term(
                "norn.shop.order.outstanding",
                "property",
                Some("norn.shop.order")
            ),
            term("norn.shop.order.theme", "property", Some("norn.shop.order")),
        ]),
        &["norn.shop.order"],
    );
    let canon = pack(
        "norn.ontology-pack.fixture-canon",
        json!([
            term("norn.canon.commitment", "class", None),
            term("norn.canon.claim", "class", None),
            term(
                "norn.canon.claim.amount",
                "property",
                Some("norn.canon.claim")
            ),
            term(
                "norn.canon.settlement.amount",
                "property",
                Some("norn.canon.claim")
            ),
        ]),
        &["norn.canon.commitment", "norn.canon.claim"],
    );
    [shop, canon, link]
        .into_iter()
        .map(|value| {
            let artifact: ArtifactEnvelope = serde_json::from_value(value).expect("envelope JSON");
            artifact.validate().expect("each artifact validates alone");
            artifact
        })
        .collect()
}

fn registry_refuses(link: Value, message: &str) {
    let diagnostics = validate_link_registry(&registry(link)).expect_err("registry must refuse");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(message)),
        "expected '{message}': {diagnostics:#?}"
    );
}

#[test]
fn a_resolved_link_reports_its_coverage() {
    let coverage = validate_link_registry(&registry(link())).expect("link resolves");
    assert_eq!(coverage.len(), 1);
    let coverage = &coverage[0];
    assert_eq!(coverage.active_terms, 3);
    assert_eq!(coverage.mapped.len(), 2);
    assert!(coverage.residual.contains("norn.shop.order.theme"));
    assert!(coverage.canon_gaps.is_empty());
}

#[test]
fn every_active_source_term_is_accounted_for() {
    let mut candidate = link();
    candidate["payload"]
        .as_object_mut()
        .expect("payload")
        .remove("residuals");
    registry_refuses(candidate, "neither a correspondence nor a residual");
}

#[test]
fn links_resolve_pinned_packs_and_active_terms_of_one_kind() {
    let mut candidate = link();
    candidate["payload"]["toPack"]["version"] = json!("0.2.0");
    registry_refuses(candidate, "the registry has @0.1.0");

    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["toTermIds"] = json!(["norn.canon.missing"]);
    registry_refuses(candidate, "not an active term of the to pack");

    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["toTermIds"] = json!(["norn.canon.claim.amount"]);
    registry_refuses(candidate, "joins terms of one kind");

    let mut candidate = link();
    candidate["payload"]["correspondences"][1]["toTermIds"] = json!(["norn.canon.claim"]);
    registry_refuses(candidate, "reads properties or predicates");
}

#[test]
fn evidence_must_be_a_mapped_source_of_the_from_pack() {
    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["evidenceRefs"] = json!(["source.shop-blog"]);
    registry_refuses(candidate, "is a study_only source");

    let mut candidate = link();
    candidate["payload"]["correspondences"][0]["evidenceRefs"] = json!(["source.other"]);
    registry_refuses(candidate, "not in the from pack's source manifest");
}
