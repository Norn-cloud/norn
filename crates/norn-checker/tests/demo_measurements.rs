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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut artifacts: Vec<ArtifactEnvelope> = [
        "registry/core/artifacts",
        "registry/packs/demo-erpnext/artifacts",
    ]
    .iter()
    .flat_map(|dir| {
        fs::read_dir(root.join(dir))
            .expect("artifacts")
            .map(|entry| {
                serde_json::from_str(
                    &fs::read_to_string(entry.expect("entry").path()).expect("read"),
                )
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
    let source = artifacts
        .iter()
        .find(|a| a.artifact_id == "demo.erpnext.measurements.source")
        .expect("source");
    let mapping = artifacts
        .iter()
        .find(|a| a.artifact_id == "demo.erpnext.measurements.mapping")
        .expect("mapping");
    assert_eq!(
        serde_json::to_value(mapping).expect("JSON")["payload"]["source"]["sha256"].as_str(),
        Some(ArtifactRef::of(source).expect("hash").sha256.as_str())
    );
    compile_mapping_artifact(mapping, &artifacts, &[])
        .unwrap_or_else(|d| panic!("admission: {d:#?}"))
}

fn row(week: i64, batch: i64, role: &str, fine_milligrams: i64, purity: &str) -> SourceDocument {
    let metal = if batch == 4 { "silver" } else { "gold" };
    // July 6 through October 19 are consecutive Mondays in UTC.
    let day = 6 + week * 7;
    let (month, date) = if day <= 31 {
        (7, day)
    } else if day <= 62 {
        (8, day - 31)
    } else if day <= 92 {
        (9, day - 62)
    } else {
        (10, day - 92)
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
    assert_eq!(gold_inputs[..48].len(), 48);
    assert_eq!(gold_inputs[48..].len(), 16);
    assert!(gold_inputs.iter().min() != gold_inputs.iter().max());
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
        assert!(
            record.links.is_empty(),
            "no canonical promotion before link qualification"
        );
        assert!(record.provenance.source.record.contains("SYN-MELT"));
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
