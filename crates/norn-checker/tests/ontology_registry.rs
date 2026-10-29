#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "focused cross-language ontology registry fixtures"
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

use norn_checker::{
    ontology_term_is_a, validate_ontology_instance, validate_ontology_instance_in_registry,
    validate_ontology_pack, validate_ontology_registry,
};
use norn_ir::JsonValue;

const COMPANY_PACK: &str =
    include_str!("../../../verification/ontology/packs/company-commerce-v0/pack.json");
const QUANTITY_PACK: &str =
    include_str!("../../../verification/ontology/packs/quantity-units-v0/pack.json");
const CONFORMANCE_FIXTURE: &str =
    include_str!("../../../verification/ontology/fixtures/registry-conformance-v0.json");

fn envelope_payload(source: &str) -> JsonValue {
    let envelope: serde_json::Value = serde_json::from_str(source).expect("pack JSON must decode");
    serde_json::from_value(envelope["payload"].clone()).expect("payload must decode")
}

fn packs() -> Vec<JsonValue> {
    vec![
        envelope_payload(COMPANY_PACK),
        envelope_payload(QUANTITY_PACK),
    ]
}

fn find_entry_mut<'a>(
    entries: &'a mut serde_json::Value,
    key: &str,
    expected: &str,
) -> &'a mut serde_json::Value {
    entries
        .as_array_mut()
        .expect("entries must be an array")
        .iter_mut()
        .find(|entry| entry.get(key).and_then(|value| value.as_str()) == Some(expected))
        .expect("entry with the expected stable identifier must exist")
}

fn find_entry_index(entries: &serde_json::Value, key: &str, expected: &str) -> usize {
    entries
        .as_array()
        .expect("entries must be an array")
        .iter()
        .position(|entry| entry.get(key).and_then(|value| value.as_str()) == Some(expected))
        .expect("entry with the expected stable identifier must exist")
}

fn field_diagnostic_path(
    prefix: &str,
    payload: &serde_json::Value,
    shape_id: &str,
    field_id: &str,
) -> String {
    let shape_index = find_entry_index(&payload["shapes"], "shapeId", shape_id);
    let shape = &payload["shapes"]
        .as_array()
        .expect("shapes must be an array")[shape_index];
    let field_index = find_entry_index(&shape["fields"], "fieldId", field_id);
    format!("{prefix}.shapes[{shape_index}].fields[{field_index}].targetTermId")
}

fn replace_pack(packs: &mut [JsonValue], pack_id: &str, replacement: JsonValue) {
    let pack = packs
        .iter_mut()
        .find(|pack| {
            pack.as_object()
                .and_then(|object| object.get("packId"))
                .and_then(|value| value.as_string())
                == Some(pack_id)
        })
        .expect("pack with the expected stable identifier must exist");
    *pack = replacement;
}

#[test]
fn registry_membership_and_instances_match_the_shared_fixture() {
    let packs = packs();
    validate_ontology_registry(&packs).expect("the canonical closed registry must validate");
    let fixture: serde_json::Value =
        serde_json::from_str(CONFORMANCE_FIXTURE).expect("fixture JSON must decode");

    for case in fixture["membershipCases"]
        .as_array()
        .expect("membership cases must be an array")
    {
        let actual = ontology_term_is_a(
            &packs,
            case["termId"].as_str().expect("termId must be text"),
            case["targetTermId"]
                .as_str()
                .expect("targetTermId must be text"),
        )
        .expect("membership registry must validate");
        assert_eq!(actual, case["expected"].as_bool().expect("expected bool"));
    }

    for case in fixture["instanceCases"]
        .as_array()
        .expect("instance cases must be an array")
    {
        let instance: JsonValue =
            serde_json::from_value(case["instance"].clone()).expect("instance must decode");
        let result = validate_ontology_instance_in_registry(&packs, &instance);
        let expected = case["expected"].as_bool().expect("expected bool");
        assert_eq!(result.is_ok(), expected, "case: {}", case["name"]);
        if let Some(expected_path) = case.get("expectedPath").and_then(serde_json::Value::as_str) {
            let diagnostics = result.expect_err("case with expectedPath must fail");
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.path.as_deref() == Some(expected_path)),
                "expected diagnostic at {expected_path}: {diagnostics:#?}"
            );
        }
    }
}

#[test]
fn cross_pack_reference_requires_the_explicit_registry() {
    let packs = packs();
    let fixture: serde_json::Value =
        serde_json::from_str(CONFORMANCE_FIXTURE).expect("fixture JSON must decode");
    let instance: JsonValue =
        serde_json::from_value(fixture["instanceCases"][0]["instance"].clone())
            .expect("instance must decode");

    let diagnostics = validate_ontology_instance(&packs[0], &instance)
        .expect_err("a single-pack checker cannot resolve a cross-pack unit reference");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path.as_deref() == Some("instance.values.measurementUnit[0]")
    }));
    validate_ontology_instance_in_registry(&packs, &instance)
        .expect("the explicit two-pack registry resolves the reference");
}

#[test]
fn registry_rejects_duplicate_ids_and_unresolved_targets() {
    let diagnostics = validate_ontology_registry(&[]).expect_err("an empty registry must fail");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path.as_deref() == Some("registry.packs"))
    );

    let mut duplicate = packs();
    let mut quantity: serde_json::Value =
        serde_json::from_str(QUANTITY_PACK).expect("quantity pack must decode");
    find_entry_mut(
        &mut quantity["payload"]["terms"],
        "termId",
        "norn.measure.unit",
    )["termId"] = serde_json::json!("norn.commerce.product");
    replace_pack(
        &mut duplicate,
        "norn.ontology-pack.quantity-units",
        serde_json::from_value(quantity["payload"].clone()).expect("payload must decode"),
    );
    let diagnostics = validate_ontology_registry(&duplicate)
        .expect_err("global duplicate term IDs must fail closed");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("globally unique"))
    );

    let mut unresolved = packs();
    let mut company: serde_json::Value =
        serde_json::from_str(COMPANY_PACK).expect("company pack must decode");
    find_entry_mut(
        &mut find_entry_mut(
            &mut company["payload"]["shapes"],
            "shapeId",
            "shape.commerce.product",
        )["fields"],
        "fieldId",
        "measurementUnit",
    )["targetTermId"] = serde_json::json!("norn.measure.unit.missing");
    let expected_path = field_diagnostic_path(
        "registry.packs[0]",
        &company["payload"],
        "shape.commerce.product",
        "measurementUnit",
    );
    replace_pack(
        &mut unresolved,
        "norn.ontology-pack.company-commerce",
        serde_json::from_value(company["payload"].clone()).expect("payload must decode"),
    );
    let diagnostics = validate_ontology_registry(&unresolved)
        .expect_err("unresolved target terms must fail closed");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.path.as_deref() == Some(expected_path.as_str()) })
    );
}

#[test]
fn pack_rejects_unbounded_term_refs_and_incompatible_hierarchy() {
    let mut company: serde_json::Value =
        serde_json::from_str(COMPANY_PACK).expect("company pack must decode");
    let target_field = find_entry_mut(
        &mut find_entry_mut(
            &mut company["payload"]["shapes"],
            "shapeId",
            "shape.commerce.product-variant",
        )["fields"],
        "fieldId",
        "parentProduct",
    );
    target_field
        .as_object_mut()
        .expect("field must be an object")
        .remove("targetTermId");
    let expected_path = field_diagnostic_path(
        "payload",
        &company["payload"],
        "shape.commerce.product-variant",
        "parentProduct",
    );
    let payload: JsonValue =
        serde_json::from_value(company["payload"].clone()).expect("payload must decode");
    let diagnostics = validate_ontology_pack(&payload)
        .expect_err("term_ref without a target membership root must fail closed");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.path.as_deref() == Some(expected_path.as_str()) })
    );

    let mut company: serde_json::Value =
        serde_json::from_str(COMPANY_PACK).expect("company pack must decode");
    find_entry_mut(
        &mut company["payload"]["terms"],
        "termId",
        "norn.commerce.product-variant",
    )["parentTermId"] = serde_json::json!("norn.identity.external-identifier");
    let payload: JsonValue =
        serde_json::from_value(company["payload"].clone()).expect("payload must decode");
    let diagnostics =
        validate_ontology_pack(&payload).expect_err("a class cannot subclass a value-object term");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("cannot be a child"))
    );
}
