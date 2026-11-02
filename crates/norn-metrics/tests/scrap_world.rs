//! DATA-FORECAST: historical evidence feeds the real statistical Function.
#![allow(clippy::expect_used, clippy::panic, clippy::print_stdout, missing_docs, reason = "synthetic acceptance scenario")]
#[path = "support/scrap_world.rs"] mod source;
use norn_ir::function::Digest;
use norn_metrics::producer::{ForecastExport,prepare_forecast};
#[test]
fn authenticated_timed_world_fits_and_october_bulk_world_retains_leakage() {
    for bulk in [false,true] {
        let world = source::world(bulk,None);
        let docs = Digest::of_json_document(br#"{"scope":"synthetic descriptive forecast","thresholdGrams":1}"#).expect("preregistered assumptions and remedy");
        let export = ForecastExport { tenant:source::SCOPE, world:"main", plan:&world.plan,rows:&world.rows,version:world.head,
            source_digest:Digest::from_hex(source::TABLE_SHA),source_head:world.head,as_of:source::micros(source::BULK_AT),complete:true,
            quarantine_count:0,assumptions_digest:docs.clone(),remedy_digest:docs,frontier_age_ms:0,fresh:true };
        let (input,_) = export.input(source::micros(source::SPLIT)).expect("input export");
        let summary = prepare_forecast(&export,&input).expect("actual library report or temporal assessment");
        assert_eq!((summary.training_rows,summary.holdout_rows),(48,16));
        assert_eq!(summary.error_unit,"g");
        assert_eq!(world.evidence.len(),128);
        for proof in &world.evidence {
            let encoded = serde_json::to_value(proof).expect("receipt-preserving export");
            assert_eq!(encoded["snapshotSha256"],proof.snapshot_sha256());
            assert_eq!(encoded["receipt"]["hash"],proof.receipt().hash);
            assert_eq!(encoded["receipt"]["body"]["logicalTime"],proof.receipt().body.logical_time);
        }
        if bulk {
            assert!(summary.temporal_leakage);
            assert_eq!((summary.late_features,summary.late_training_labels),(64,48));
            assert!(summary.rmse_nanos.is_none() && summary.report.is_none());
        } else {
            assert!(!summary.temporal_leakage);
            assert_eq!((summary.late_features,summary.late_training_labels),(0,0));
            assert!(summary.rmse_nanos.is_some() && summary.report.is_some());
            println!("timed world: 48 train / 16 holdout; RMSE nanos {:?}; diagnostics {}",summary.rmse_nanos,summary.diagnostics_passed);
        }
        assert_eq!(world.store.verified_head().expect("read-only analysis").commits,world.head);
    }
}
