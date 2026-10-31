//! Fixed synthetic holdout, with future labels excluded from training.
#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "independent held-out error oracle"
)]
use norn_checker::EffectiveCatalog;
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use norn_metrics::{
    analysis::forecast,
    temporal::{TemporalPlan, TemporalRow, TemporalTable, TemporalValue},
};
fn table(offset: i64) -> TemporalTable {
    let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("prepare typed fixtures");
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(&root).join("metric-demand.json")).expect("fixture"),
    )
    .expect("artifacts");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let plan = TemporalPlan::compile(&catalog, "demo.Demand", &["stock".into()], "units", 0)
        .expect("plan");
    let value = |n: i64, at| TemporalValue {
        value: RuntimeValue::from_json_text(n.to_string()).expect("number"),
        valid_at: at,
        recorded_at: at,
        source: format!("synthetic:{n}:{at}"),
    };
    let rows = [4, 4, 8, 8, 12, 14, 16]
        .into_iter()
        .zip([100, 200, 300, 400, 500, 700, 800])
        .enumerate()
        .map(|(i, (y, at))| TemporalRow {
            key: format!("holdout-{i}"),
            at,
            features: vec![value(i64::try_from(i).expect("index") + 1, at)],
            outcome: value(offset + y, at + 1),
        })
        .collect();
    plan.materialize(801, 42, rows).expect("table")
}
#[test]
fn chronological_holdout_matches_independent_scores_without_training_on_future_rows() {
    let table = table(0);
    let report = forecast(&table, 600).expect("forecast");
    assert_eq!(report.source_head, 42);
    assert_eq!(report.boundary, 600);
    assert_eq!(report.fit.n, 5);
    assert_eq!(report.evaluation_n, 2);
    assert!((report.fit.slope - 2.0).abs() < 1e-12);
    assert!((report.fit.intercept - 1.2).abs() < 1e-12);
    assert!((report.rmse - 0.8).abs() < 1e-12);
    assert!((report.baseline_rmse - 61.84_f64.sqrt()).abs() < 1e-12);
    for (p, expected) in report.predictions.iter().zip([13.2, 15.2]) {
        assert!((p.2 - expected).abs() < 1e-12);
        assert!(p.1 >= 600);
    }
    assert!(forecast(&table, 0).is_err());
    assert!(forecast(&table, 1000).is_err());
    assert!(
        forecast(&table, 501).is_err(),
        "a training label learned at the boundary is embargoed"
    );
}
#[test]
fn holdout_scores_are_invariant_under_a_representable_outcome_translation() {
    let reference = forecast(&table(0), 600).expect("reference");
    let translated = forecast(&table(10_000_000_000_000_000), 600).expect("translated");
    assert!((reference.rmse - translated.rmse).abs() < 1e-12);
    assert!((reference.baseline_rmse - translated.baseline_rmse).abs() < 1e-12);
}
