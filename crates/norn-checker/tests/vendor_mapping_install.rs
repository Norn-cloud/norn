#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "synthetic install invariant assertions"
)]

//! Install invariant (bounded evidence): a vendor mapping uses only authored
//! field requirements, and every requirement agrees with authenticated tenant
//! discovery at an exact supported revision. Failure precedes any cell write.
//! Generic mappings make no claim about vendor parity.

use norn_checker::mapping::vendor::{
    VENDOR_MAPPING_SLOT, check_vendor_mapping, validate_discovered_schema,
};
use norn_ir::mapping::MappingIr;
use norn_ir::source_schema::SourceSchemaIr;
use serde_json::{Value, json};

fn schema() -> SourceSchemaIr {
    serde_json::from_value(json!({
        "format":"norn.source-schema/1", "source":{"system":"ToyVendor","instance":"https://tenant.invalid","version":"1.2.3"},
        "observedAt":"2026-10-28T00:00:00Z", "adapter":{"name":"toy.discovery","version":"1.0.0"},
        "rawMetadata":{"uri":"tenant://discovery","sha256":"a".repeat(64)},
        "datasets":[{"id":"Rows","nativeName":"Rows","kind":"document"}],
        "fields":[{"dataset":"Rows","name":"id","ordinal":0,"nativeType":"Text","typeCandidate":{"type":"string","origin":"declared"},"required":true}],
        "constraints":[{"id":"pk","kind":"primary_key","dataset":"Rows","fields":["id"],"origin":"declared"}]
    })).expect("synthetic schema")
}

fn mapping() -> MappingIr {
    let mut wire = json!({
        "format":"norn.mapping/1", "source":{"schemaId":"toy.schema","version":"1.0.0","sha256":"b".repeat(64)},
        "interfaces":["toy.interface"],"maps":[{"id":"row","source":{"dataset":"Rows"},"target":{"entity":"toy.Row","interface":"toy.interface"},"subject":"input.id","sourceRecord":"input.id"}]
    });
    wire["extensions"] = json!({ VENDOR_MAPPING_SLOT: {
        "system":"ToyVendor", "versions":["1.2.3"],
        "apiTerms":{"uri":"https://toy.invalid/api-terms","sha256":"c".repeat(64)}
    }});
    serde_json::from_value(wire).expect("mapping")
}

#[test]
fn authored_sparse_contract_accepts_extra_fields_only_in_tenant_discovery() {
    let mapping = mapping();
    let expected = schema();
    let mut discovered = expected.clone();
    let mut extra = discovered.fields[0].clone();
    extra.name = "unmapped".into();
    extra.ordinal = norn_ir::WholeNumber(1);
    discovered.fields.push(extra);
    assert!(check_vendor_mapping(&mapping, &expected).is_empty());
    validate_discovered_schema(&mapping, &expected, &discovered).expect("sparse subset");
    assert!(
        !check_vendor_mapping(&mapping, &discovered).is_empty(),
        "a shipped full-schema payload fails"
    );
}

#[test]
fn missing_patch_malformed_and_unsupported_versions_fail_closed() {
    for version in [
        None,
        Some("1.2"),
        Some("1.2.4"),
        Some("1.2.3 "),
        Some("^1.2.3"),
    ] {
        let mut discovered = schema();
        discovered.source.version = version.map(str::to_owned);
        assert!(
            validate_discovered_schema(&mapping(), &schema(), &discovered).is_err(),
            "{version:?}"
        );
    }
    for versions in [
        json!([]),
        json!(["1.2"]),
        json!(["^1.2.3"]),
        json!(["1.2.3", "1.2.3"]),
        Value::Null,
    ] {
        let mut wire = serde_json::to_value(mapping()).expect("wire");
        wire["extensions"][VENDOR_MAPPING_SLOT]["versions"] = versions;
        let mapping =
            serde_json::from_value(wire).expect("extension remains unchecked data until checker");
        assert!(!check_vendor_mapping(&mapping, &schema()).is_empty());
    }
}

#[test]
fn removed_changed_nullable_identity_and_wrong_source_are_refused() {
    let expected = schema();
    let mut cases = Vec::new();
    let mut removed = expected.clone();
    removed.fields.clear();
    cases.push(removed);
    let mut changed = expected.clone();
    changed.fields[0].native_type = "Integer".into();
    cases.push(changed);
    let mut nullable = expected.clone();
    nullable.fields[0].required = Some(false);
    cases.push(nullable);
    let mut identity = expected.clone();
    identity.constraints.clear();
    cases.push(identity);
    let mut instance = expected.clone();
    instance.source.instance = "https://other.invalid".into();
    cases.push(instance);
    let mut namespace = expected.clone();
    namespace.datasets[0].namespace = Some("other_namespace".into());
    cases.push(namespace);
    let mut system = expected.clone();
    system.source.system = "OtherVendor".into();
    cases.push(system);
    for discovered in cases {
        assert!(validate_discovered_schema(&mapping(), &expected, &discovered).is_err());
    }
}

#[test]
fn opaque_reads_and_expressive_schema_metadata_cannot_hide_in_sparse_packs() {
    let mut opaque = mapping();
    opaque.maps[0].subject = norn_ir::ZenExpression("input".into());
    assert!(!check_vendor_mapping(&opaque, &schema()).is_empty());
    let mut expected = schema();
    expected.fields[0].description = Some("vendor prose".into());
    assert!(!check_vendor_mapping(&mapping(), &expected).is_empty());
}

#[test]
fn generic_authored_mappings_do_not_assert_vendor_version_parity() {
    let mut mapping = mapping();
    mapping.extensions = None;
    let mut schema = schema();
    schema.source.version = None;
    assert!(check_vendor_mapping(&mapping, &schema).is_empty());
    validate_discovered_schema(&mapping, &schema, &schema).expect("generic mapping unchanged");
}

#[test]
fn arbitrary_extensions_cannot_carry_a_hidden_schema_dump() {
    let mut wire = serde_json::to_value(mapping()).expect("wire");
    wire["extensions"]["hidden.schema/1.0.0"] = json!({"fullSchema":schema()});
    let mapping = serde_json::from_value(wire).expect("mapping");
    assert!(!check_vendor_mapping(&mapping, &schema()).is_empty());
}

#[test]
fn explicit_generic_registry_contract_remains_unqualified_and_cannot_mix_with_vendor() {
    let mut wire = serde_json::to_value(mapping()).expect("wire");
    wire["extensions"][norn_checker::mapping::vendor::GENERIC_MAPPING_SLOT] =
        json!({"parity":"unqualified"});
    let mixed = serde_json::from_value(wire.clone()).expect("mapping");
    assert!(!check_vendor_mapping(&mixed, &schema()).is_empty());
    wire["extensions"]
        .as_object_mut()
        .expect("extensions")
        .remove(VENDOR_MAPPING_SLOT);
    let generic = serde_json::from_value(wire).expect("mapping");
    assert!(check_vendor_mapping(&generic, &schema()).is_empty());
}

#[test]
fn nested_mapping_requires_the_discovered_parent_child_identity() {
    let mut expected = serde_json::to_value(schema()).expect("schema");
    expected["datasets"]
        .as_array_mut()
        .expect("datasets")
        .push(json!({"id":"Lines","nativeName":"Lines","kind":"child_document"}));
    expected["fields"].as_array_mut().expect("fields").extend([
        json!({"dataset":"Rows","name":"children","ordinal":1,"nativeType":"Table","typeCandidate":{"type":"json","origin":"declared"},"required":false}),
        json!({"dataset":"Lines","name":"parent","ordinal":0,"nativeType":"Text","typeCandidate":{"type":"string","origin":"declared"},"required":true}),
        json!({"dataset":"Lines","name":"label","ordinal":1,"nativeType":"Text","typeCandidate":{"type":"string","origin":"declared"},"required":true})]);
    expected["constraints"].as_array_mut().expect("constraints").push(json!({"id":"ownership","kind":"parent_child","dataset":"Lines","fields":["parent"],"target":{"dataset":"Rows","fields":["id"]},"collection":"children","origin":"declared"}));
    let expected: SourceSchemaIr = serde_json::from_value(expected).expect("schema");
    let mut wire = serde_json::to_value(mapping()).expect("mapping");
    wire["maps"][0]["properties"] = json!([{"property":"label","value":"input.label","nested":{"dataset":"Lines","field":"children","where":"input.label != ''"}}]);
    let mapping = serde_json::from_value(wire).expect("nested mapping");
    assert!(check_vendor_mapping(&mapping, &expected).is_empty());
    validate_discovered_schema(&mapping, &expected, &expected)
        .expect("mapped ownership fields are structural requirements");
    let mut changed = expected.clone();
    changed.constraints.retain(|constraint| {
        constraint.kind != norn_ir::source_schema::ConstraintKind::ParentChild
    });
    assert!(validate_discovered_schema(&mapping, &expected, &changed).is_err());
}

#[test]
fn exact_api_revisions_are_supported_without_turning_them_into_major_ranges() {
    for revision in ["2026-10", "v60.0"] {
        let mut wire = serde_json::to_value(mapping()).expect("mapping");
        wire["extensions"][VENDOR_MAPPING_SLOT]["versions"] = json!([revision]);
        let mapping = serde_json::from_value(wire).expect("mapping");
        let mut discovered = schema();
        discovered.source.version = Some(revision.into());
        validate_discovered_schema(&mapping, &discovered, &discovered).expect("exact API revision");
    }
}
