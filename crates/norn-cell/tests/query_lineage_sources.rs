//! DATA-LINEAGE / LINEAGE-SUM-1: canonical contributions resolve through admitted bundles.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "source lineage conformance"
)]
mod query_support;
use norn_cell::compute::{
    QueryContext,
    lineage::{ContributionSource, LineageDisclosure},
};
use norn_cell::{CellStore, InstalledDefinitionInput, MAIN_WORLD, Model, SqliteCell};
use norn_ir::{ArtifactEnvelope, RuntimeValue as V};
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use query_support::*;
use std::collections::{BTreeMap, BTreeSet};
const SCOPE: &str = "org_demo/dev";
const WEEK: &str = "\"2026-10-19\"";
type Host =
    SqliteCell<norn_kernel::ObservationEvent, norn_ir::RuntimeObject, norn_ir::RuntimeObject>;
fn artifacts() -> Vec<ArtifactEnvelope> {
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
        "../../../fixtures/ir/lineage/scrap-yield.artifacts.json"
    ))
    .expect("typed authoring output");
    let source = artifacts
        .iter()
        .find(|a| a.artifact_id == "demo.scrap.source")
        .expect("source");
    let pin = norn_checker::ArtifactRef::of(source).expect("exact source pin");
    let mapping = artifacts
        .iter_mut()
        .find(|a| a.artifact_id == "demo.scrap.mapping")
        .expect("mapping");
    let mut binding = serde_json::to_value(&*mapping).expect("compiled declaration");
    binding["payload"]["source"]["sha256"] = serde_json::Value::String(pin.sha256);
    *mapping = serde_json::from_value(binding).expect("bind compiled source identity");
    artifacts.push(
        serde_json::from_str(include_str!(
            "../../../registry/core/artifacts/core.units.json"
        ))
        .expect("pinned unit catalog"),
    );
    artifacts
}
fn store(path: &std::path::Path) -> CellStore {
    let artifacts = artifacts();
    let context = norn_checker::EntityArtifactSetContext {
        registry: &BTreeSet::default(),
        base: &[],
        tenant: None,
    };
    let findings = norn_checker::check_artifact_set(&artifacts, &context);
    assert!(findings.is_empty(), "{findings:#?}");
    let mut objects = Vec::new();
    let mut interfaces = Vec::new();
    for artifact in &artifacts {
        let value = serde_json::to_value(artifact).expect("generated definition");
        if let Some(object) = value["payload"].get("objectType") {
            objects.push(serde_json::from_value(object.clone()).expect("object"));
        }
        if let Some(interface) = value["payload"].get("interface") {
            interfaces.push(serde_json::from_value(interface.clone()).expect("interface"));
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &[], &interfaces).expect("model");
    let mut store =
        CellStore::open(path, model.clone(), norn_cell::StoreOptions::default()).expect("store");
    let bytes = norn_ir::canonical::canonical_json_of(&artifacts).expect("canonical definitions");
    store
        .install_definitions(
            model,
            InstalledDefinitionInput {
                canonical_set: String::from_utf8(bytes).expect("UTF8"),
                registry: vec![],
                receipt_sha256: "synthetic-lineage-admission".into(),
                admission_receipt: None,
                readmission: None,
                diff: serde_json::json!({}),
                installed_at: CLOCK - 1,
                layout: None,
            },
            None,
            SCOPE,
        )
        .expect("retained mapping definitions");
    store
}
fn raw(fields: &[&str], index: usize) -> Observed<V> {
    let id = fields[2];
    let number =
        |text: &str| V::from(norn_ir::Decimal::parse(&format!("{text}e-3")).expect("exact grams"));
    let payload = norn_ir::runtime_object! {
        "id"=>id, "source_row"=>format!("external-row/{index}"),
        "input"=>number(fields[4]), "recovered"=>number(fields[5]), "loss"=>number(fields[6]),
        "occurred_at"=>fields[8], "metal"=>fields[3],
        "catalog"=>norn_ir::runtime_object! {"tags"=>vec![V::from("recycled"), V::from("gold")], "label"=>"Catalog enrichment"},
    };
    let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(
        &norn_ir::canonical::canonical_json_of(&payload).expect("source payload"),
    );
    let document = format!("ERP-DOC/{index}");
    Observed {
        id: ObservationId(format!("transport/{index}")),
        provenance: Provenance {
            source: "synthetic-erpnext".into(),
            source_id: document.clone(),
            mapping_version: "scrap/1".into(),
            observed_at: LogicalTime::from_millis(CLOCK),
        },
        input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"synthetic-erpnext", "source_id"=>document,
        "event_id"=>index.to_string(), "source_version"=>"1", "pipeline_version"=>"scrap/1", "kind"=>"upsert",
        "extracted_at"=>CLOCK, "payload"=>payload, "payload_digest"=>digest},
    }
}
fn loaded() -> (
    Host,
    norn_checker::metric::compute::QuestionPlan,
    QueryContext,
    Vec<Observed<V>>,
    tempfile::TempDir,
) {
    loaded_with_extra_source_bytes(0)
}
fn loaded_with_extra_source_bytes(
    extra: usize,
) -> (
    Host,
    norn_checker::metric::compute::QuestionPlan,
    QueryContext,
    Vec<Observed<V>>,
    tempfile::TempDir,
) {
    let directory = tempfile::tempdir().expect("directory");
    let mut store = store(&directory.path().join("cell.sqlite"));
    let mut ids = Vec::new();
    let mut replayed = None;
    let mut raws = Vec::new();
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "demo.scrap.mapping".into(),
        dataset: "conversions".into(),
        connector_instance: "demo".into(),
        partition: "measurements".into(),
        recorded_at: norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into()),
    };
    for (index, line) in include_str!("../../../fixtures/demo/scrap-yield-k1.csv")
        .lines()
        .skip(1)
        .enumerate()
    {
        let fields: Vec<_> = line.split(',').collect();
        let mut raw = raw(&fields, index);
        if extra > 0 && fields[2] == "SYN-MELT-15-1" {
            let payload = raw
                .input
                .as_object_mut()
                .expect("envelope")
                .get_mut("payload")
                .expect("payload");
            payload
                .as_object_mut()
                .expect("record")
                .insert("extra_source_evidence".into(), V::from("x".repeat(extra)));
            let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(
                &norn_ir::canonical::canonical_json_of(payload).expect("sealed payload"),
            );
            raw.input
                .as_object_mut()
                .expect("envelope")
                .insert("payload_digest".into(), V::from(digest));
        }
        // The cell computes both the mapping and its bundle identity before processing.
        let (id, _) = store
            .inject_mapping_bundle(&raw, &input)
            .expect("sealed capture");
        if fields[2] == "SYN-MELT-15-1" {
            let mut duplicate = raw.clone();
            duplicate.id.0 = "redelivered transport".into();
            let (same, inserted) = store
                .inject_mapping_bundle(&duplicate, &input)
                .expect("redelivery");
            assert_eq!(same, id);
            assert!(!inserted, "redelivery adds no numeric instance");
            replayed = Some(id.clone());
        }
        ids.push(id);
        raws.push(raw);
    }
    let mut cell = SqliteCell::new(store, SCOPE, |_| BTreeMap::new()).expect("host");
    for id in ids {
        assert!(
            cell.process_mapping_bundle(&id)
                .expect("mapping processing")
        );
    }
    let head = cell.store().verified_head().expect("processed head");
    assert!(
        !cell
            .process_mapping_bundle(replayed.as_deref().expect("contributing source row"))
            .expect("processed redelivery")
    );
    assert_eq!(
        cell.store().verified_head().expect("no duplicate commit"),
        head
    );
    let (_, plan, mut context) = fixture();
    context.world = MAIN_WORLD.into();
    context.scope = SCOPE.into();
    context.mapping_pin = "demo.scrap.mapping/0.1.0".into();
    (cell, plan, context, raws, directory)
}
#[test]
fn scrap_yield_drills_from_294_over_300_to_exact_authorized_source_rows_and_documents() {
    let (cell, plan, context, raws, _directory) = loaded();
    let store = cell.store();
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("exact");
    let trace = store
        .trace_question(
            &plan,
            context,
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(true),
            },
        )
        .expect("source trace");
    assert_eq!(trace.result, result);
    assert_eq!(
        trace.contributions.len(),
        4,
        "catalog tags are not numeric contributions"
    );
    assert_eq!(trace.result.rows[WEEK].totals[0].canonical(), "294");
    assert_eq!(trace.result.rows[WEEK].totals[1].canonical(), "300");
    let mapping = artifacts()
        .into_iter()
        .find(|a| a.artifact_id == "demo.scrap.mapping")
        .expect("mapping declaration");
    let pin = norn_checker::ArtifactRef::of(&mapping).expect("mapping identity");
    for contribution in trace.contributions {
        let ContributionSource::Resolved { provenance } = contribution.source else {
            panic!("mapped source edge required")
        };
        let raw = raws
            .iter()
            .find(|r| {
                r.input.as_object().expect("envelope")["payload"]
                    .as_object()
                    .expect("payload")["id"]
                    .as_str()
                    == Some(&contribution.fact.key)
            })
            .expect("sealed source row");
        assert_eq!(
            provenance.source_record_ref.record,
            format!(
                "conversions/{}",
                raw.input.as_object().expect("envelope")["payload"]
                    .as_object()
                    .expect("payload")["source_row"]
                    .as_str()
                    .expect("semantic source row")
            )
        );
        assert_eq!(provenance.document_ref.record, raw.provenance.source_id);
        assert_eq!(
            provenance.document_digest,
            raw.input.as_object().expect("envelope")["payload_digest"]
                .as_str()
                .expect("digest")
        );
        assert_eq!(provenance.mapping_pin.artifact_id, "demo.scrap.mapping");
        assert_eq!(provenance.mapping_pin.version, "0.1.0");
        assert_eq!(provenance.mapping_pin.sha256, pin.sha256);
        assert_eq!(provenance.source_record_ref.system, "synthetic-erpnext");
        assert_eq!(provenance.document_ref.system, raw.provenance.source);
        assert_eq!(provenance.bundle_digest.len(), 64);
        assert!(
            provenance
                .field_provenance
                .contains_key(&norn_ir::PropertyId("recovered".into()))
        );
        let proof = store
            .read()
            .fact_version_evidence_with_budget(
                MAIN_WORLD,
                TYPE,
                &contribution.fact.key,
                contribution.fact.version,
                &mut norn_cell::reactive::DecodeBudget::new(
                    &norn_cell::reactive::ReactiveLimits::from_profile(
                        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
                    ),
                ),
            )
            .expect("numeric proof")
            .expect("retained");
        assert_eq!(
            contribution.fact_digest.as_deref(),
            Some(proof.snapshot_sha256())
        );
        assert_eq!(contribution.receipt_hash, proof.receipt().hash);
    }
}
#[test]
fn source_disclosure_is_independent_of_canonical_values_and_numeric_membership() {
    let (cell, plan, context, _, _directory) = loaded();
    let store = cell.store();
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("exact");
    for values_allowed in [false, true] {
        for source_allowed in [false, true] {
            let trace = store
                .trace_question(
                    &plan,
                    context.clone(),
                    &result.lineage_ref,
                    WEEK,
                    |_| Ok(true),
                    LineageDisclosure {
                        values: &mut |_| Ok(values_allowed),
                        source: &mut |_| Ok(source_allowed),
                    },
                )
                .expect("independent disclosure");
            assert_eq!(trace.result, result);
            let encoded = serde_json::to_string(&trace).expect("wire");
            assert_eq!(encoded.contains("ERP-DOC/"), source_allowed);
            for contribution in trace.contributions {
                assert_eq!(
                    matches!(contribution.source, ContributionSource::Resolved { .. }),
                    source_allowed
                );
                assert_eq!(
                    matches!(contribution.source, ContributionSource::Restricted),
                    !source_allowed
                );
                assert_eq!(contribution.restricted, !values_allowed);
                assert_eq!(contribution.values.is_some(), values_allowed);
                assert_eq!(contribution.fact_digest.is_some(), values_allowed);
            }
        }
    }
    let head = store.verified_head().expect("head");
    assert!(matches!(
        store.trace_question(
            &plan,
            context,
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Err(norn_cell::CellError::ComputeRefused(
                    "synthetic.source_policy_unavailable"
                ))
            }
        ),
        Err(norn_cell::CellError::ComputeRefused(
            "synthetic.source_policy_unavailable"
        ))
    ));
    assert_eq!(store.verified_head().expect("unchanged"), head);
}

#[test]
fn source_grants_apply_to_each_canonical_contribution_identity() {
    let (cell, plan, context, _, _directory) = loaded();
    let result = cell
        .store()
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("exact");
    let trace = cell
        .store()
        .trace_question(
            &plan,
            context,
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |row| Ok(row.id == "SYN-MELT-15-1"),
                source: &mut |row| Ok(row.id != "SYN-MELT-15-1"),
            },
        )
        .expect("per-object disclosure");
    assert_eq!(trace.result, result);
    assert_eq!(trace.contributions.len(), 4);
    for contribution in trace.contributions {
        let selected = contribution.fact.key == "SYN-MELT-15-1";
        assert_eq!(contribution.values.is_some(), selected);
        assert_eq!(contribution.fact_digest.is_some(), selected);
        assert_eq!(contribution.restricted, !selected);
        assert_eq!(
            matches!(contribution.source, ContributionSource::Restricted),
            selected
        );
        assert_eq!(
            matches!(contribution.source, ContributionSource::Resolved { .. }),
            !selected
        );
    }
}

#[test]
fn source_bundle_edge_survives_redelivery_and_real_cell_restart() {
    let (cell, plan, context, _, directory) = loaded();
    let result = cell
        .store()
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("exact");
    let first = cell
        .store()
        .trace_question(
            &plan,
            context.clone(),
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(true),
            },
        )
        .expect("source trace");
    let model = cell.store().model().clone();
    drop(cell);
    let store = CellStore::open(
        &directory.path().join("cell.sqlite"),
        model,
        norn_cell::StoreOptions::default(),
    )
    .expect("restart");
    let resumed = store
        .trace_question(
            &plan,
            context,
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(true),
            },
        )
        .expect("authenticated source after restart");
    assert_eq!(
        serde_json::to_value(first).expect("before"),
        serde_json::to_value(resumed).expect("after")
    );
}
#[test]
fn source_budget_exhaustion_refuses_instead_of_returning_a_partial_trace() {
    let (cell, plan, context, _, _directory) = loaded_with_extra_source_bytes(500_000);
    let result = cell
        .store()
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("numeric result");
    let hidden = cell
        .store()
        .trace_question(
            &plan,
            context.clone(),
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(false),
            },
        )
        .expect("restricted source requires no document decode");
    assert_eq!(hidden.result, result);
    assert!(matches!(
        cell.store().trace_question(
            &plan,
            context,
            &result.lineage_ref,
            WEEK,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(true)
            }
        ),
        Err(norn_cell::CellError::MappingBundleMismatch)
    ));
}
#[test]
fn corrupt_source_journal_refuses_while_numeric_membership_remains_pinned() {
    let (cell, plan, context, _, directory) = loaded();
    let store = cell.store();
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("exact");
    let id = "SYN-MELT-15-1";
    let receipt = store
        .read()
        .receipt_for_fact_version(MAIN_WORLD, TYPE, id, result.rows[WEEK].members[id].version)
        .expect("emitting receipt");
    let seq = receipt.body.commit_seq;
    let connection = rusqlite::Connection::open(directory.path().join("cell.sqlite"))
        .expect("storage corruption seam");
    assert_eq!(
        connection
            .execute(
                "UPDATE _norn_export_journal SET hash=?1 WHERE commit_seq=?2",
                rusqlite::params!["0".repeat(64), i64::try_from(seq).expect("sequence")]
            )
            .expect("corrupt source export"),
        1
    );
    drop(connection);
    let head = store.verified_head().expect("immutable numeric frontier");
    assert!(
        matches!(store.trace_question(&plan,context,&result.lineage_ref,WEEK,|_|Ok(true), LineageDisclosure { values: &mut |_|Ok(true), source: &mut |_|Ok(true) }),Err(norn_cell::CellError::LedgerCorrupt{seq:at,..}) if at==seq)
    );
    assert_eq!(store.verified_head().expect("no write"), head);
}
