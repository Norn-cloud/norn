#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
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
use norn_checker::{validate_ontology_instance, validate_ontology_pack};
use norn_ir::{ArtifactEnvelope, JsonValue};

const PACK_FIXTURE: &str =
    include_str!("../../../verification/ontology/packs/company-commerce-v0/pack.json");
const OWL_PROJECTION: &str =
    include_str!("../../../verification/ontology/packs/company-commerce-v0/ontology.owl.ttl");
const SHACL_PROJECTION: &str =
    include_str!("../../../verification/ontology/packs/company-commerce-v0/shapes.shacl.ttl");

fn fixture_json() -> serde_json::Value {
    serde_json::from_str(PACK_FIXTURE).expect("ontology fixture is valid JSON")
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

#[test]
fn pinned_company_commerce_ontology_pack_validates() {
    validate_fixture(fixture_json()).expect("pinned ontology pack should validate");
}

#[test]
fn ontology_pack_requires_opaque_pack_identity_and_literal_kind() {
    for (path, replacement) in [
        ("packId", serde_json::Value::Null),
        ("packId", serde_json::json!("shopify.category")),
        ("packKind", serde_json::Value::Null),
        ("packKind", serde_json::json!("composition")),
    ] {
        let mut candidate = fixture_json();
        let payload = candidate["payload"]
            .as_object_mut()
            .expect("fixture payload must be an object");
        if replacement.is_null() {
            payload.remove(path);
        } else {
            payload.insert(path.to_string(), replacement);
        }

        let payload: JsonValue = serde_json::from_value(candidate["payload"].clone())
            .expect("payload must decode as JsonValue");
        let diagnostics = validate_ontology_pack(&payload)
            .expect_err("invalid ontology identity/kind must be rejected");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.path.as_deref() == Some(&format!("payload.{path}"))
            }),
            "expected a diagnostic at payload.{path}: {diagnostics:#?}"
        );
    }
}

#[test]
fn ontology_pack_rejects_floating_external_releases() {
    for release_id in ["latest", "release-2026", "nightly", "v1"] {
        let mut candidate = fixture_json();
        let source_index = candidate["payload"]["sources"]
            .as_array()
            .expect("sources must be an array")
            .iter()
            .position(|source| source["sourceId"] == "source.norn-synthetic-taxonomy")
            .expect("Shopify source must exist");
        candidate["payload"]["sources"][source_index]["releaseId"] = serde_json::json!(release_id);

        let diagnostics =
            validate_fixture(candidate).expect_err("non-immutable releases must be rejected");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref()
                == Some(&format!("payload.sources[{source_index}].releaseId"))
                && diagnostic.message.contains("immutable")
        }));
    }
}

fn source_index(candidate: &serde_json::Value, source_id: &str) -> usize {
    candidate["payload"]["sources"]
        .as_array()
        .expect("sources must be an array")
        .iter()
        .position(|source| source["sourceId"] == source_id)
        .expect("source must exist")
}

fn assert_source_diagnostic(
    candidate: serde_json::Value,
    path: &str,
    message: &str,
) -> Vec<norn_ir::Diagnostic> {
    let diagnostics = validate_fixture(candidate).expect_err("source manifest must be rejected");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some(path) && diagnostic.message.contains(message)
        }),
        "expected '{message}' at {path}: {diagnostics:#?}"
    );
    diagnostics
}

#[test]
fn source_manifest_requires_a_closed_import_mode() {
    let index = source_index(&fixture_json(), "source.schema-org");
    for replacement in [None, Some(serde_json::json!("copy"))] {
        let mut candidate = fixture_json();
        let source = candidate["payload"]["sources"][index]
            .as_object_mut()
            .expect("source must be an object");
        match replacement {
            None => {
                source.remove("importMode");
            }
            Some(value) => {
                source.insert("importMode".to_owned(), value);
            }
        }
        let path = format!("payload.sources[{index}].importMode");
        let message = if source.contains_key("importMode") {
            "one of"
        } else {
            "required"
        };
        assert_source_diagnostic(candidate, &path, message);
    }
}

#[test]
fn imported_source_names_its_importer_and_has_a_redistributable_license() {
    let index = source_index(&fixture_json(), "source.norn-synthetic-taxonomy");

    let mut candidate = fixture_json();
    candidate["payload"]["sources"][index]
        .as_object_mut()
        .expect("source must be an object")
        .remove("importer");
    assert_source_diagnostic(
        candidate,
        &format!("payload.sources[{index}].importer"),
        "must name its importer",
    );

    let mut candidate = fixture_json();
    candidate["payload"]["sources"][index]["license"] = serde_json::json!("GPL-3.0");
    candidate["payload"]["sources"][index]["redistribution"] = serde_json::json!("reference_only");
    assert_source_diagnostic(
        candidate,
        &format!("payload.sources[{index}].license"),
        "map it instead",
    );

    for (field, value, message) in [
        ("name", "Frappe", "lowercase dotted identifier"),
        ("version", "latest", "strict semantic version"),
    ] {
        let mut candidate = fixture_json();
        candidate["payload"]["sources"][index]["importer"][field] = serde_json::json!(value);
        assert_source_diagnostic(
            candidate,
            &format!("payload.sources[{index}].importer.{field}"),
            message,
        );
    }
}

#[test]
fn mapped_source_is_content_pinned_by_file_or_release_hash() {
    let index = source_index(&fixture_json(), "source.schema-org");
    let mut unpinned = fixture_json();
    unpinned["payload"]["sources"][index]
        .as_object_mut()
        .expect("source must be an object")
        .remove("files");
    assert_source_diagnostic(
        unpinned.clone(),
        &format!("payload.sources[{index}].releaseId"),
        "content-pinned",
    );

    unpinned["payload"]["sources"][index]["releaseId"] =
        serde_json::json!("30.0@0f0c97a4f666b2f8563573fe48453782fd51b87a504523cf0c9aff6a71c3eec4");
    validate_fixture(unpinned).expect("a hash-pinned release id is a content pin");
}

#[test]
fn mapping_cannot_cite_a_study_only_source() {
    let mut candidate = fixture_json();
    let index = source_index(&candidate, "source.schema-org");
    candidate["payload"]["sources"][index]["importMode"] = serde_json::json!("study_only");
    let mapping = candidate["payload"]["mappings"]
        .as_array()
        .expect("mappings must be an array")
        .iter()
        .position(|mapping| mapping["sourceId"] == "source.schema-org")
        .expect("a schema.org mapping must exist");
    assert_source_diagnostic(
        candidate,
        &format!("payload.mappings[{mapping}].sourceId"),
        "study_only",
    );
}

fn vocabulary_term(term_id: &str, kind: &str, domain: Option<&str>) -> serde_json::Value {
    let mut term = serde_json::json!({
        "termId": term_id,
        "label": term_id,
        "definition": "A vocabulary handle used by test mutations.",
        "termKind": kind,
        "status": "active",
        "semanticAuthority": "semantic_only"
    });
    if let Some(domain) = domain {
        term["domainTermId"] = serde_json::json!(domain);
    }
    term
}

fn with_terms(terms: &[serde_json::Value]) -> serde_json::Value {
    let mut candidate = fixture_json();
    candidate["payload"]["terms"]
        .as_array_mut()
        .expect("terms must be an array")
        .extend(terms.iter().cloned());
    candidate
}

fn expect_term_diagnostic(candidate: serde_json::Value, message: &str) {
    let diagnostics = validate_fixture(candidate).expect_err("vocabulary rule must refuse");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(message)),
        "expected '{message}': {diagnostics:#?}"
    );
}

#[test]
fn action_property_and_predicate_handles_need_no_shape() {
    validate_fixture(with_terms(&[
        vocabulary_term(
            "norn.commerce.order.place",
            "action",
            Some("norn.commerce.order"),
        ),
        vocabulary_term(
            "norn.commerce.order.total",
            "property",
            Some("norn.commerce.order"),
        ),
        vocabulary_term("norn.commerce.guard.order.empty", "predicate", None),
    ]))
    .expect("vocabulary handles validate without shapes");
}

#[test]
fn members_and_operations_name_an_active_class_domain() {
    expect_term_diagnostic(
        with_terms(&[vocabulary_term("norn.commerce.order.place", "action", None)]),
        "names the class it belongs to",
    );
    expect_term_diagnostic(
        with_terms(&[vocabulary_term(
            "norn.commerce.order.total",
            "property",
            Some("norn.commerce.order.place"),
        )]),
        "must resolve to an active class term",
    );
    expect_term_diagnostic(
        with_terms(&[vocabulary_term(
            "norn.commerce.order.kind",
            "class",
            Some("norn.commerce.order"),
        )]),
        "has no domainTermId",
    );
}

#[test]
fn vocabulary_handles_take_no_closed_shape() {
    let mut candidate = with_terms(&[vocabulary_term(
        "norn.commerce.guard.order.empty",
        "predicate",
        None,
    )]);
    candidate["payload"]["shapes"]
        .as_array_mut()
        .expect("shapes must be an array")
        .push(serde_json::json!({
            "shapeId": "shape.guard.order.empty",
            "termId": "norn.commerce.guard.order.empty",
            "closed": true,
            "fields": [{"fieldId": "x", "valueType": "string", "minCount": 0, "maxCount": 1}]
        }));
    expect_term_diagnostic(candidate, "takes no closed shape");
}

#[test]
fn commerce_pack_covers_schema_classes_and_order_item_cardinality() {
    let candidate = fixture_json();
    let terms = candidate["payload"]["terms"]
        .as_array()
        .expect("terms must be an array");
    for term_id in [
        "norn.commerce.product",
        "norn.commerce.offer",
        "norn.commerce.order",
        "norn.commerce.order-item",
        "norn.company.organization",
        "norn.company.person",
        "norn.company.postal-address",
    ] {
        assert!(
            terms.iter().any(|term| term["termId"] == term_id),
            "missing required schema-backed term {term_id}"
        );
    }
    let order_shape = candidate["payload"]["shapes"]
        .as_array()
        .expect("shapes must be an array")
        .iter()
        .find(|shape| shape["termId"] == "norn.commerce.order")
        .expect("Order shape must exist");
    let line_items = order_shape["fields"]
        .as_array()
        .expect("Order fields must be an array")
        .iter()
        .find(|field| field["fieldId"] == "lineItems")
        .expect("Order.lineItems must exist");
    assert_eq!(line_items["valueType"], "term_ref");
    assert_eq!(line_items["targetTermId"], "norn.commerce.order-item");
    assert_eq!(line_items["minCount"], 1);
    assert_eq!(line_items["maxCount"], 100);
}

#[test]
fn ontology_pack_rejects_inactive_shapes_and_instances() {
    let mut candidate = fixture_json();
    let terms = candidate["payload"]["terms"]
        .as_array_mut()
        .expect("terms must be an array");
    let trade_item = terms
        .iter_mut()
        .find(|term| term["termId"] == "norn.logistics.trade-item")
        .expect("trade-item term must exist");
    trade_item["status"] = serde_json::json!("deprecated");

    let diagnostics = validate_fixture(candidate.clone())
        .expect_err("inactive terms must not retain promotion shapes");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains("promotion shapes may target active terms only")
    }));

    candidate["payload"]["shapes"]
        .as_array_mut()
        .expect("shapes must be an array")
        .retain(|shape| shape["termId"] != "norn.logistics.trade-item");
    validate_fixture(candidate.clone())
        .expect("deprecated term without a shape remains historical metadata");
    let artifact: ArtifactEnvelope =
        serde_json::from_value(candidate).expect("mutated fixture must decode");
    let instance: JsonValue = serde_json::from_value(serde_json::json!({
        "instanceId": "trade-item.historical",
        "termId": "norn.logistics.trade-item",
        "values": {},
        "provenance": {
            "sourceKind": "imported_claim",
            "sourceRef": "source.norn-synthetic-taxonomy",
            "observedAt": "2026-08-22T00:00:00Z",
            "evidenceRefs": ["https://example.invalid/evidence/product-historical"]
        }
    }))
    .expect("instance must decode");
    let payload = artifact.payload.to_json_value();
    let diagnostics = validate_ontology_instance(&payload, &instance)
        .expect_err("historical instances require an explicit mode that does not exist yet");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("instance.termId")
            && diagnostic.message.contains("active ontology term")
    }));
}

#[test]
fn unicode_whitespace_is_rejected_consistently_in_text_and_iris() {
    let mut candidate = fixture_json();
    candidate["payload"]["terms"][0]["label"] = serde_json::json!("\u{0085}");
    candidate["payload"]["mappings"][0]["externalIri"] =
        serde_json::json!("https://example.com/\u{0085}mapping");
    let diagnostics = validate_fixture(candidate)
        .expect_err("Unicode-only text and whitespace-bearing IRIs must fail");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.path.as_deref() == Some("payload.terms[0].label") })
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].externalIri")
    }));
}

#[test]
fn ontology_pack_cannot_smuggle_transition_authority() {
    for forbidden_field in ["capabilities", "effects", "transitionGuards", "roles"] {
        let mut candidate = fixture_json();
        candidate["payload"]["terms"][0][forbidden_field] = serde_json::json!(["grant_everything"]);

        let diagnostics = validate_fixture(candidate)
            .expect_err("ontology terms must reject authority-bearing fields");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some(&format!("payload.terms[0].{forbidden_field}"))
                && diagnostic.message.contains("unknown field")
        }));
    }
}

#[test]
fn exact_external_mapping_requires_explicit_human_ratification() {
    let mut candidate = fixture_json();
    candidate["payload"]["mappings"][0]["relation"] = serde_json::json!("exact_match");

    let diagnostics = validate_fixture(candidate)
        .expect_err("unratified exact semantic mapping must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].ratifiedBy")
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].ratifiedAt")
    }));
}

#[test]
fn ontology_mapping_requires_reviewed_confidence_provenance() {
    for field in ["confidenceBasisPoints", "reviewedBy", "reviewedAt"] {
        let mut candidate = fixture_json();
        candidate["payload"]["mappings"][0]
            .as_object_mut()
            .expect("mapping must be an object")
            .remove(field);
        let diagnostics = validate_fixture(candidate)
            .expect_err("missing mapping review provenance must be rejected");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some(&format!("payload.mappings[0].{field}"))
        }));
    }

    for replacement in [
        serde_json::json!(-1),
        serde_json::json!(10_001),
        serde_json::json!(1.5),
    ] {
        let mut candidate = fixture_json();
        candidate["payload"]["mappings"][0]["confidenceBasisPoints"] = replacement;
        let diagnostics = validate_fixture(candidate)
            .expect_err("confidence must be an integer in the closed basis-point range");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.path.as_deref() == Some("payload.mappings[0].confidenceBasisPoints")
                && diagnostic.message.contains("0 through 10000")
        }));
    }

    for boundary in [0, 10_000] {
        let mut candidate = fixture_json();
        candidate["payload"]["mappings"][0]["confidenceBasisPoints"] = serde_json::json!(boundary);
        validate_fixture(candidate).expect("both basis-point boundaries must be admitted");
    }

    let mut candidate = fixture_json();
    candidate["payload"]["mappings"][0]["reviewedBy"] = serde_json::json!("\u{2003}");
    let diagnostics = validate_fixture(candidate)
        .expect_err("a whitespace-only reviewer identity must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].reviewedBy")
    }));

    let mut candidate = fixture_json();
    candidate["payload"]["mappings"][0]["reviewedAt"] = serde_json::json!("2026-02-30T25:61:61Z");
    let diagnostics = validate_fixture(candidate)
        .expect_err("review timestamps outside the shared UTC subset must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].reviewedAt")
            && diagnostic.message.contains("UTC RFC3339")
    }));
}

fn candidate_with_mapping_replacement() -> serde_json::Value {
    let mut candidate = fixture_json();
    let mut replacement = candidate["payload"]["mappings"][0].clone();
    replacement["mappingId"] = serde_json::json!("mapping.party.schema-organization.v2");
    replacement["reviewedAt"] = serde_json::json!("2026-08-24T01:00:00Z");
    candidate["payload"]["mappings"][0]["supersededByMappingId"] = replacement["mappingId"].clone();
    candidate["payload"]["mappings"]
        .as_array_mut()
        .expect("mappings must be an array")
        .push(replacement);
    candidate
}

#[test]
fn ontology_mapping_allows_one_direct_same_term_same_source_replacement() {
    validate_fixture(candidate_with_mapping_replacement())
        .expect("a direct replacement preserving term and pinned source must validate");

    let mut exact = candidate_with_mapping_replacement();
    let last = exact["payload"]["mappings"]
        .as_array()
        .expect("mappings must be an array")
        .len()
        - 1;
    for index in [0, last] {
        exact["payload"]["mappings"][index]["relation"] = serde_json::json!("exact_match");
        exact["payload"]["mappings"][index]["ratifiedBy"] =
            serde_json::json!("principal.norn-ontology-maintainers");
        exact["payload"]["mappings"][index]["ratifiedAt"] =
            serde_json::json!("2026-08-24T03:00:00Z");
    }
    validate_fixture(exact)
        .expect("a superseded exact claim must not conflict with its one active replacement");
}

#[test]
fn ontology_mapping_supersession_rejects_invalid_graph_edges() {
    let mut unresolved = fixture_json();
    unresolved["payload"]["mappings"][0]["supersededByMappingId"] =
        serde_json::json!("mapping.missing");
    let diagnostics = validate_fixture(unresolved)
        .expect_err("an unresolved mapping replacement must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].supersededByMappingId")
            && diagnostic.message.contains("does not resolve")
    }));

    let mut self_replacement = fixture_json();
    self_replacement["payload"]["mappings"][0]["supersededByMappingId"] =
        self_replacement["payload"]["mappings"][0]["mappingId"].clone();
    let diagnostics =
        validate_fixture(self_replacement).expect_err("a self-replacing mapping must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].supersededByMappingId")
            && diagnostic.message.contains("cannot supersede itself")
    }));

    let mut cross_term = fixture_json();
    cross_term["payload"]["mappings"][0]["supersededByMappingId"] =
        cross_term["payload"]["mappings"][3]["mappingId"].clone();
    let diagnostics = validate_fixture(cross_term)
        .expect_err("a replacement for a different Norn term must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].supersededByMappingId")
            && diagnostic.message.contains("preserve the exact Norn term")
    }));

    let mut cross_source = candidate_with_mapping_replacement();
    let last = cross_source["payload"]["mappings"]
        .as_array()
        .expect("mappings must be an array")
        .len()
        - 1;
    cross_source["payload"]["mappings"][last]["sourceId"] =
        serde_json::json!("source.norn-synthetic-taxonomy");
    let diagnostics = validate_fixture(cross_source)
        .expect_err("a replacement from a different pinned source must be rejected");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].supersededByMappingId")
            && diagnostic.message.contains("pinned source identity")
    }));

    let mut chained = candidate_with_mapping_replacement();
    let mut terminal = chained["payload"]["mappings"]
        .as_array()
        .expect("mappings must be an array")
        .last()
        .expect("replacement mapping must exist")
        .clone();
    terminal["mappingId"] = serde_json::json!("mapping.party.schema-organization.v3");
    terminal["reviewedAt"] = serde_json::json!("2026-08-24T02:00:00Z");
    let mappings = chained["payload"]["mappings"]
        .as_array_mut()
        .expect("mappings must be an array");
    mappings.last_mut().expect("replacement mapping must exist")["supersededByMappingId"] =
        terminal["mappingId"].clone();
    mappings.push(terminal);
    let diagnostics = validate_fixture(chained)
        .expect_err("a supersession chain must be collapsed to a direct active replacement");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("payload.mappings[0].supersededByMappingId")
            && diagnostic.message.contains("directly to an active mapping")
    }));
}

#[test]
fn closed_instance_checker_accepts_valid_values_and_rejects_unknown_fields() {
    let artifact: ArtifactEnvelope =
        serde_json::from_str(PACK_FIXTURE).expect("fixture must decode as ArtifactEnvelope");
    let payload = artifact.payload.to_json_value();
    let valid: JsonValue = serde_json::from_value(serde_json::json!({
        "instanceId": "product.example-1",
        "termId": "norn.commerce.product",
        "values": {
            "name": ["Example product"],
            "description": ["Fixture instance"],
            "externalIdentifiers": [{"scheme": "sku", "value": "ABC-1", "issuer": "norn-test"}],
            "categories": ["shopify:sg-1"]
        },
        "provenance": {
            "sourceKind": "imported_claim",
            "sourceRef": "source.norn-synthetic-taxonomy",
            "observedAt": "2026-08-22T00:00:00Z",
            "evidenceRefs": ["https://example.invalid/evidence/product-1"]
        }
    }))
    .expect("instance must decode as JsonValue");
    validate_ontology_instance(&payload, &valid).expect("valid closed instance should pass");

    let invalid: JsonValue = serde_json::from_value(serde_json::json!({
        "instanceId": "product.example-2",
        "termId": "norn.commerce.product",
        "values": {
            "name": ["Example product"],
            "authorizesTransition": [true]
        },
        "provenance": {
            "sourceKind": "imported_claim",
            "sourceRef": "source.norn-synthetic-taxonomy",
            "observedAt": "2026-08-22T00:00:00Z",
            "evidenceRefs": ["https://example.invalid/evidence/product-2"]
        }
    }))
    .expect("instance must decode as JsonValue");
    let diagnostics = validate_ontology_instance(&payload, &invalid)
        .expect_err("closed shape must reject authority-smuggling fields");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("instance.values.authorizesTransition")
    }));
}

#[test]
fn owl_and_shacl_files_are_projection_only_contracts() {
    assert!(OWL_PROJECTION.contains("owl:versionIRI"));
    assert!(!OWL_PROJECTION.contains("owl:imports"));
    assert!(!OWL_PROJECTION.contains("authorizesTransition"));
    assert!(!OWL_PROJECTION.contains("grantsCapability"));

    assert!(SHACL_PROJECTION.contains("sh:closed true"));
    assert!(SHACL_PROJECTION.contains("differential-oracle projection"));
    assert!(!SHACL_PROJECTION.contains("authorizesTransition"));
    assert!(!SHACL_PROJECTION.contains("grantsCapability"));
}
