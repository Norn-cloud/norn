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
use norn_cell::compute::lineage::{ContributionSource, LineageDisclosure};
use norn_cell::{CellStore, MAIN_WORLD};
use query_support::*;
#[path = "support/query_lineage.rs"]
mod lineage_support;
use lineage_support::*;
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
            provenance
                .document_digest
                .as_deref()
                .expect("values granted"),
            raw.input.as_object().expect("envelope")["payload_digest"]
                .as_str()
                .expect("digest")
        );
        assert_eq!(provenance.mapping_pin.artifact_id, "demo.scrap.mapping");
        assert_eq!(provenance.mapping_pin.version, "0.1.0");
        assert_eq!(provenance.mapping_pin.sha256, pin.sha256);
        assert_eq!(provenance.source_record_ref.system, "synthetic-erpnext");
        assert_eq!(provenance.document_ref.system, raw.provenance.source);
        assert_eq!(provenance.bundle_digest.as_deref().map(str::len), Some(64));
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
                if let ContributionSource::Resolved { provenance } = &contribution.source {
                    assert_eq!(provenance.document_digest.is_some(), values_allowed);
                    assert_eq!(provenance.bundle_digest.is_some(), values_allowed);
                }
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
fn value_derived_source_digests_follow_the_values_grant_not_the_sources_grant() {
    let (cell, plan, context, _, _directory) = loaded();
    let result = cell
        .store()
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("exact");
    let trace_with = |values_allowed: bool| {
        cell.store()
            .trace_question(
                &plan,
                context.clone(),
                &result.lineage_ref,
                WEEK,
                |_| Ok(true),
                LineageDisclosure {
                    values: &mut |_| Ok(values_allowed),
                    source: &mut |_| Ok(true),
                },
            )
            .expect("trace")
    };
    // Population-only caller: Sources granted, Values denied. The payload digest
    // and bundle digest hash the withheld values, so they are absent on the
    // wire, while the permitted source identities and mapping stay.
    let population = trace_with(false);
    assert_eq!(population.contributions.len(), 4);
    for contribution in &population.contributions {
        let ContributionSource::Resolved { provenance } = &contribution.source else {
            panic!("sources granted")
        };
        assert!(provenance.document_digest.is_none() && provenance.bundle_digest.is_none());
        assert!(!provenance.document_ref.record.is_empty());
        assert_eq!(provenance.mapping_pin.artifact_id, "demo.scrap.mapping");
        assert!(!provenance.field_provenance.is_empty());
    }
    let wire = serde_json::to_string(&population).expect("wire");
    assert!(!wire.contains("documentDigest") && !wire.contains("bundleDigest"));
    assert!(
        wire.contains("ERP-DOC/"),
        "unrelated Sources disclosure stays"
    );
    // With Values granted both digests are disclosed again.
    let full = trace_with(true);
    for contribution in &full.contributions {
        let ContributionSource::Resolved { provenance } = &contribution.source else {
            panic!("sources granted")
        };
        assert_eq!(
            provenance.document_digest.as_deref().map(str::len),
            Some(64)
        );
        assert_eq!(provenance.bundle_digest.as_deref().map(str::len), Some(64));
    }
    assert!(
        serde_json::to_string(&full)
            .expect("wire")
            .contains("documentDigest")
    );
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
        // D53: the bucket mixes restricted and readable members, so no member's
        // values or digest is disclosed; the restricted flag stays per member.
        assert!(contribution.values.is_none() && contribution.fact_digest.is_none());
        if let ContributionSource::Resolved { provenance } = &contribution.source {
            assert!(provenance.document_digest.is_none() && provenance.bundle_digest.is_none());
        }
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
        Err(norn_cell::CellError::ComputeRefused(
            "core.compute.bound_exceeded"
        ))
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
