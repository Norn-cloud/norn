#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! `MappingIR` (T8-MAP-1a, #2600): the published JSON Schema is checked in
//! and current, the `ERPNext` fixture decodes and round-trips, and malformed
//! documents (`tests/fixtures/mapping/invalid/validation.invalid_document.*`)
//! fail to decode; the other invalid fixtures are `norn-checker` findings.

use std::{fs, path::Path};

use norn_ir::mapping::{MappingIr, mapping_ir_schema};

const UPDATE_ENV: &str = "NORN_IR_UPDATE_PAYLOAD_SCHEMAS";

fn fixture_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mapping")
}

fn read_json(path: &Path) -> serde_json::Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} should be JSON: {error}", path.display()))
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "CI schema output when artifact storage is full"
)]
fn mapping_ir_schema_is_checked_in_and_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/mapping_ir.schema.json");
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(&mapping_ir_schema()).expect("schema should serialize")
    );
    if std::env::var_os(UPDATE_ENV).is_some() {
        if fs::read_to_string(&path).ok().as_deref() != Some(rendered.as_str()) {
            println!(
                "---BEGIN mapping_ir.schema.json---\n{rendered}---END mapping_ir.schema.json---"
            );
        }
        fs::write(&path, &rendered).expect("schema should write");
    }
    let checked_in: Option<serde_json::Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let current: serde_json::Value =
        serde_json::from_str(&rendered).expect("rendered schema should parse");
    assert!(
        checked_in.as_ref() == Some(&current),
        "{} is stale; regenerate with {UPDATE_ENV}=1 cargo test -p norn-ir --test mapping_ir\n\
         ---BEGIN mapping_ir.schema.json---\n{rendered}---END mapping_ir.schema.json---",
        path.display()
    );
}

#[test]
fn erpnext_fixture_decodes_and_round_trips() {
    let wire = read_json(&fixture_dir().join("erpnext.mapping.json"));
    let mapping: MappingIr =
        serde_json::from_value(wire.clone()).expect("the `ERPNext` mapping should decode");
    let encoded = serde_json::to_value(&mapping).expect("mapping should encode");
    assert_eq!(encoded, wire, "the mapping should round-trip");
    assert_eq!(mapping.source.schema_id, "erpnext.source_schema");
    let order = mapping.map("sales_order").expect("sales_order is declared");
    assert_eq!(
        order.transform_id("total_amount"),
        "sales_order.total_amount"
    );
}

#[test]
fn fashion_pivot_fixture_decodes_and_round_trips() {
    let wire = read_json(&fixture_dir().join("fashion-size-color.mapping.json"));
    let mapping: MappingIr =
        serde_json::from_value(wire.clone()).expect("the Fashion pivot mapping should decode");
    let encoded = serde_json::to_value(&mapping).expect("mapping should encode");
    assert_eq!(encoded, wire, "the pivot fields should round-trip");
    let item = mapping
        .map("fashion_item")
        .expect("fashion_item is declared");
    assert_eq!(
        item.when.as_ref().map(norn_ir::ZenExpression::as_str),
        Some("input.variant_of != null")
    );
    assert_eq!(
        item.properties
            .get(2)
            .expect("size is the third property")
            .nested
            .as_ref()
            .map(|nested| nested.predicate.as_str()),
        Some("input.attribute == 'Size'")
    );
}

#[test]
fn malformed_documents_fail_to_decode() {
    let mut seen = 0_u32;
    for entry in fs::read_dir(fixture_dir().join("invalid")).expect("fixtures should be listable") {
        let path = entry.expect("entry should be readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("fixture names are UTF-8")
            .to_owned();
        let decoded = serde_json::from_value::<MappingIr>(read_json(&path));
        if name.starts_with("validation.invalid_document.") {
            assert!(decoded.is_err(), "{name} should fail to decode");
            seen = seen.saturating_add(1);
        } else {
            assert!(
                decoded.is_ok(),
                "{name} should decode (its finding is the checker's): {decoded:?}"
            );
        }
    }
    assert!(seen >= 2, "every malformed-document fixture was exercised");
}

#[test]
fn vendor_mapping_contract_is_typed_without_changing_generic_wire_shape() {
    let contract: norn_ir::mapping::VendorMappingContract =
        serde_json::from_value(serde_json::json!({
            "system":"ToyVendor", "versions":["1.2.3"],
            "apiTerms":{"uri":"https://toy.invalid/terms","sha256":"a".repeat(64)}
        }))
        .expect("authored contract");
    assert_eq!(contract.versions, ["1.2.3"]);
    let mut invalid = serde_json::to_value(contract).expect("contract wire");
    invalid["fullSchema"] = serde_json::json!({});
    assert!(serde_json::from_value::<norn_ir::mapping::VendorMappingContract>(invalid).is_err());
}

fn link_record() -> norn_ir::mapping::MappedRecord {
    use norn_ir::mapping::{ActionInvocation, LinkPatch, MappedRecord};
    use norn_ir::{LinkId, ObjectTypeName};
    let provenance: norn_ir::InstanceProvenance = serde_json::from_value(serde_json::json!({
        "source":{"system":"synthetic-a","record":"rows/one"},
        "mappingVersion":"synthetic.mapping@1", "sourceFields":["target"],
        "transform":"row.target", "recordedAt":"2026-11-03T00:00:00Z"
    }))
    .expect("provenance");
    MappedRecord {
        map: "row".into(),
        object_type: ObjectTypeName("test.Source".into()),
        key: "one".into(),
        invocations: vec![ActionInvocation {
            action_id: "observe".into(),
            input: Default::default(),
        }],
        lifecycle_snapshots: vec![],
        links: vec![LinkPatch {
            object_type: ObjectTypeName("test.Source".into()),
            key: "one".into(),
            link: LinkId("target".into()),
            target_type: ObjectTypeName("test.Target".into()),
            target_key: "two".into(),
            provenance: provenance.clone(),
        }],
        cleared_links: vec![LinkId("optional".into())],
        property_provenance: Default::default(),
        provenance,
    }
}

#[test]
fn obs_link_identity_retains_explicit_targets_and_nullable_clears() {
    let record = link_record();
    let claims = record.link_claims("mapping-bundle:first");
    assert_eq!(claims.len(), 2);
    let target = claims
        .iter()
        .find(|c| c.role.as_str() == "target")
        .expect("target");
    assert_eq!(
        target.target.as_ref().expect("explicit endpoint").key,
        "two"
    );
    assert_eq!(
        target.provenance,
        record.links.first().expect("patch").provenance
    );
    let clear = claims
        .iter()
        .find(|c| c.role.as_str() == "optional")
        .expect("clear");
    assert!(clear.target.is_none());
    assert_eq!(clear.provenance.transform.as_deref(), Some("row.optional"));
    assert_ne!(target.id().expect("id"), clear.id().expect("id"));
    let mut changed = target.clone();
    changed.provenance.source.system = "synthetic-b".into();
    assert_ne!(
        target.id().expect("id"),
        changed.id().expect("other source")
    );
    changed = target.clone();
    changed.provenance.mapping_version.0 = "synthetic.mapping@2".into();
    assert_ne!(
        target.id().expect("id"),
        changed.id().expect("other mapping pin")
    );
    changed = target.clone();
    changed.observation_id = "mapping-bundle:second".into();
    assert_ne!(
        target.id().expect("id"),
        changed.id().expect("other observation")
    );
    let encoded = serde_json::to_value(target).expect("encode");
    assert_eq!(
        serde_json::from_value::<norn_ir::mapping::ObservedLinkClaim>(encoded).expect("decode"),
        *target
    );
}

proptest::proptest! {
    #[test]
    fn obs_link_claim_ids_are_order_and_redelivery_independent(
        targets in proptest::collection::vec("[a-z]{1,12}", 1..20), repeats in 1usize..5,
    ) {
        use std::collections::BTreeSet;
        let mut record = link_record();
        let template = record.links.first().expect("patch").clone();
        record.links = targets.into_iter().map(|key| {
            let mut patch = template.clone(); patch.target_key = key; patch
        }).collect();
        let expected: BTreeSet<_> = record.link_claims("observation").iter().map(|c| c.id().expect("id")).collect();
        record.links.reverse();
        let repeated: BTreeSet<_> = (0..repeats).flat_map(|_| record.link_claims("observation"))
            .map(|c| c.id().expect("id")).collect();
        proptest::prop_assert_eq!(expected, repeated);
    }
}
