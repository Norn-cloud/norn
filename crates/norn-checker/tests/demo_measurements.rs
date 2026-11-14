#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "synthetic mapping conformance"
)]

use std::{fs, path::Path};

use norn_checker::{
    ArtifactRef,
    mapping::{CompiledMapping, SourceDocument, compile_mapping_artifact},
};
use norn_ir::{ArtifactEnvelope, RuntimeValue, provenance::UtcTimestamp};

fn compiled() -> CompiledMapping {
    compile("demo-erpnext", "demo.erpnext.measurements.mapping")
}

fn compile(pack: &str, mapping_id: &str) -> CompiledMapping {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut artifacts: Vec<ArtifactEnvelope> = [
        root.join("registry/core/artifacts"),
        root.join(format!("registry/packs/{pack}/artifacts")),
    ]
    .iter()
    .flat_map(|dir| {
        fs::read_dir(dir).expect("artifacts").map(|entry| {
            serde_json::from_str(&fs::read_to_string(entry.expect("entry").path()).expect("read"))
                .expect("artifact")
        })
    })
    .collect();
    artifacts.retain(|a| {
        a.artifact_id.starts_with("demo.")
            || [
                "core.value_types.entity",
                "std.value_types.entity",
                "core.Agent.interface",
                "core.AgentRole.interface",
                "core.Resource.interface",
                "core.EconomicEvent.interface",
                "core.Commitment.interface",
                "core.Hold.interface",
                "core.LedgerEffect.interface",
                "core.Claim.interface",
            ]
            .contains(&a.artifact_id.as_str())
    });
    let mapping = artifacts
        .iter()
        .find(|a| a.artifact_id == mapping_id)
        .expect("mapping");
    let value = serde_json::to_value(mapping).expect("JSON");
    let source = artifacts
        .iter()
        .find(|a| Some(a.artifact_id.as_str()) == value["payload"]["source"]["schemaId"].as_str())
        .expect("source");
    assert_eq!(
        serde_json::to_value(mapping).expect("JSON")["payload"]["source"]["sha256"].as_str(),
        Some(ArtifactRef::of(source).expect("hash").sha256.as_str())
    );
    compile_mapping_artifact(mapping, &artifacts, &[])
        .unwrap_or_else(|d| panic!("admission: {d:#?}"))
}

#[test]
fn d1b_shipment_identity_converges_with_independent_evidence() {
    let erp = compile("demo-commerce", "demo.erpnext.identity.mapping");
    let shopify = compile("demo-commerce", "demo.shopify.identity.mapping");
    let recorded = UtcTimestamp("2026-10-26T00:00:00Z".into());
    let delivery = SourceDocument::from_json(
        "Delivery Note",
        r#"{
        "name":"SYN-DN-001", "custom_shipment_ref":"demo/shipment/SYN-001"
    }"#,
        None,
    )
    .expect("delivery");
    let fulfillment = SourceDocument::from_json(
        "Fulfillment",
        r#"{
        "id":"SYN-FULFILLMENT-001", "custom_shipment_ref":"demo/shipment/SYN-001"
    }"#,
        None,
    )
    .expect("fulfillment");
    let a = erp
        .apply(std::slice::from_ref(&delivery), &recorded)
        .expect("ERP mapping");
    let b = shopify
        .apply(std::slice::from_ref(&fulfillment), &recorded)
        .expect("Shopify mapping");
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert_eq!(
        (&a[0].object_type, &a[0].key),
        (&b[0].object_type, &b[0].key)
    );
    assert_eq!(a[0].key, "demo/shipment/SYN-001");
    assert_ne!(a[0].provenance.source, b[0].provenance.source);
    assert_ne!(
        a[0].provenance.mapping_version,
        b[0].provenance.mapping_version
    );
    assert!(a[0].links.is_empty() && b[0].links.is_empty());
    assert_eq!(
        b,
        shopify
            .apply(&[fulfillment], &recorded)
            .expect("Shopify first/replay")
    );
    assert_eq!(
        a,
        erp.apply(&[delivery], &recorded)
            .expect("ERP second/replay")
    );
}

#[test]
fn d1b_catalog_not_holding_and_unresolved_references() {
    let erp = compile("demo-commerce", "demo.erpnext.identity.mapping");
    let shopify = compile("demo-commerce", "demo.shopify.identity.mapping");
    let recorded = UtcTimestamp("2026-10-26T00:00:00Z".into());
    let item = SourceDocument::from_json(
        "Item",
        r#"{
        "name":"SYN-ITEM", "item_code":"SHARED-SKU", "custom_catalog_ref":"demo/catalog/GOLD"
    }"#,
        None,
    )
    .expect("item");
    let variant = SourceDocument::from_json(
        "ProductVariant",
        r#"{
        "id":"SYN-VARIANT", "sku":"SHARED-SKU", "custom_catalog_ref":"demo/catalog/SILVER"
    }"#,
        None,
    )
    .expect("variant");
    let a = erp.apply(&[item], &recorded).expect("catalog item");
    let b = shopify
        .apply(&[variant], &recorded)
        .expect("catalog variant");
    assert_eq!(a[0].object_type.as_str(), "demo.commerce.CatalogEntry");
    assert_eq!(a[0].object_type, b[0].object_type);
    assert_ne!(a[0].key, b[0].key, "same SKU cannot resolve identity");
    assert_eq!(
        a[0].invocations[0].input.len(),
        2,
        "id and objectId only; no Holding quantity"
    );
    for (reference, code) in [
        ("", "mapping.runtime.invalid_record"),
        (
            ",\"custom_shipment_ref\":null",
            "mapping.runtime.invalid_record",
        ),
        (
            ",\"custom_shipment_ref\":\"\"",
            "mapping.runtime.invalid_key",
        ),
        (
            ",\"custom_shipment_ref\":[\"A\",\"B\"]",
            "mapping.runtime.invalid_record",
        ),
    ] {
        for (mapping, dataset, field) in [
            (&shopify, "Fulfillment", "id"),
            (&erp, "Delivery Note", "name"),
        ] {
            let document = SourceDocument::from_json(
                dataset,
                &format!("{{\"{field}\":\"SYN-UNRESOLVED\"{reference}}}"),
                None,
            )
            .expect("raw evidence");
            let findings = mapping
                .apply(&[document], &recorded)
                .expect_err("unresolved reference");
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].code, code);
            assert_eq!(
                findings[0].span,
                Some(norn_checker::Span::pointer("/documents/0"))
            );
        }
    }
}

fn row(week: i64, batch: i64, role: &str, fine_milligrams: i64, purity: &str) -> SourceDocument {
    let metal = if batch == 4 { "silver" } else { "gold" };
    // July 6 through October 19 are consecutive Mondays in UTC.
    let day = week
        .checked_mul(7)
        .and_then(|offset| offset.checked_add(6))
        .expect("bounded week");
    let (month, date) = if day <= 31 {
        (7, day)
    } else if day <= 62 {
        (8, day.checked_sub(31).expect("August"))
    } else if day <= 92 {
        (9, day.checked_sub(62).expect("September"))
    } else {
        (10, day.checked_sub(92).expect("October"))
    };
    let occurred_at = format!("2026-{month:02}-{date:02}T12:00:00Z");
    let grams = format!("{}.{:03}", fine_milligrams / 1000, fine_milligrams % 1000);
    SourceDocument::from_json("Stock Entry Measurement", &format!(r#"{{
        "name":"SYN-{week:02}-{batch}-{role}", "parent":"SYN-MELT-{week:02}-{batch}",
        "custom_metal":"{metal}", "custom_role":"{role}", "custom_lot":"SYN-LOT-{week:02}-{batch}-{role}",
        "custom_gross_grams":{grams}, "custom_purity":{purity}, "custom_fine_grams":{grams},
        "custom_occurred_at":"{occurred_at}"
    }}"#), Some(UtcTimestamp(occurred_at))).expect("synthetic document")
}

#[test]
fn d1a_exact_measurements_history_and_showcase() {
    let mapping = compiled();
    let recorded = UtcTimestamp("2026-10-26T00:00:00Z".into());
    let mut docs = Vec::new();
    let mut gold_inputs = Vec::new();
    let mut residuals = std::collections::BTreeSet::new();
    for week in 0..16 {
        for batch in 0..5 {
            let input = if week == 15 {
                [60_000, 75_000, 80_000, 85_000, 50_000][usize::try_from(batch).expect("batch")]
            } else {
                50_000 + batch * 7_000 + (week * 193 + batch * 71) % 997
            };
            let loss = if week == 15 {
                input / 50
            } else {
                input / 50 + (week * 17 + batch * 13) % 103
            };
            if batch < 4 {
                gold_inputs.push(input);
                residuals.insert(loss - input / 50);
            }
            for (role, fine) in [("input", input), ("output", input - loss), ("loss", loss)] {
                docs.push(row(week, batch, role, fine, "1.000000"));
            }
        }
    }
    let records = mapping
        .apply(&docs, &recorded)
        .expect("all measurements map");
    assert_eq!(records.len(), 240);
    assert_eq!(gold_inputs.len(), 64);
    let gold_times = records
        .iter()
        .filter(|record| {
            let values = &record.invocations[0].input;
            values["metal"].as_str() == Some("gold") && values["role"].as_str() == Some("input")
        })
        .map(|record| {
            record.invocations[0].input["occurred_at"]
                .as_str()
                .expect("measurement timestamp")
        })
        .collect::<Vec<_>>();
    assert_eq!(gold_times.len(), 64);
    assert!(gold_times[..48].iter().max() < gold_times[48..].iter().min());
    assert_ne!(gold_inputs.iter().min(), gold_inputs.iter().max());
    assert!(residuals.len() > 1);
    let showcase = mapping
        .apply(
            &[
                row(15, 0, "output", 58_800, "1.000000"),
                row(15, 1, "output", 73_500, "1.000000"),
                row(15, 2, "output", 78_400, "1.000000"),
                row(15, 3, "output", 83_300, "1.000000"),
            ],
            &recorded,
        )
        .expect("showcase");
    let outputs = showcase
        .iter()
        .map(|r| {
            let RuntimeValue::Number(n) = &r.invocations[0].input["fine_grams"] else {
                panic!("exact decimal")
            };
            n.to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs, ["58.8", "73.5", "78.4", "83.3"]);
    assert_eq!(gold_inputs[60..].iter().sum::<i64>(), 300_000);
    for record in &records {
        assert_eq!(
            record.links,
            [],
            "no canonical promotion before link qualification"
        );
        let mut parts = record.provenance.source.record.rsplitn(3, '/');
        let child = parts.next().expect("child row");
        let parent = parts.next().expect("parent document");
        assert!(parts.next().is_some(), "dataset prefix");
        assert!(parent.starts_with("SYN-MELT-"));
        assert!(child.starts_with("SYN-") && !child.starts_with("SYN-MELT-"));
    }
    let replay = mapping.apply(&docs, &recorded).expect("replay");
    assert_eq!(records, replay);
}

#[test]
fn d1a_evidence_only_preserves_bad_purity_and_refuses_missing_fields() {
    let mapping = compiled();
    let recorded = UtcTimestamp("2026-10-26T00:00:00Z".into());
    let bad = mapping
        .apply(&[row(15, 0, "input", 60_000, "1.20")], &recorded)
        .expect("D3 evaluates laws later");
    assert_eq!(
        bad[0].invocations[0].input["purity"],
        RuntimeValue::Number("1.2".parse().expect("decimal"))
    );
    let absent = SourceDocument::from_json(
        "Stock Entry Measurement",
        r#"{"name":"SYN-INCOMPLETE"}"#,
        None,
    )
    .expect("JSON");
    assert!(mapping.apply(&[absent], &recorded).is_err());
}

#[test]
fn d3_purity_law_governs_the_concrete_measurement_type() {
    let artifact: ArtifactEnvelope = serde_json::from_str(include_str!(
        "../../../registry/packs/demo-erpnext/artifacts/demo.erpnext.purity.law.json"
    ))
    .expect("generated law");
    let value = serde_json::to_value(artifact).expect("law envelope");
    let law: norn_ir::LawIR =
        serde_json::from_value(value["payload"].clone()).expect("law declaration");
    assert_eq!(law.subject, "demo.erpnext.StockEntryMeasurement");
    assert_eq!(
        law.scope.root,
        norn_ir::LawRoot::Entity(law.subject.clone())
    );
    assert_eq!(law.scope.object_types, [law.subject]);
    assert_eq!(law.severity, norn_ir::LawSeverity::Error);
    assert!(matches!(law.modality, norn_ir::LawModality::Always {}));
    compiled();
}
