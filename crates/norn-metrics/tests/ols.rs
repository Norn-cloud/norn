//! Synthetic OLS reference with exact, orthogonal residuals.
#![allow(clippy::expect_used, clippy::arithmetic_side_effects, reason = "independent numerical regression oracle")]
use norn_checker::EffectiveCatalog;
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use norn_metrics::{analysis::ols, temporal::{TemporalPlan, TemporalRow, TemporalValue}};

#[test]
fn regression_has_known_fit_and_retains_diagnostic_and_plot_evidence() {
    let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("prepare typed fixtures");
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_slice(&std::fs::read(std::path::Path::new(&root).join("metric-demand.json")).expect("fixture")).expect("artifacts");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let plan = TemporalPlan::compile(&catalog, "demo.Demand", &["stock".into()], "units", 0).expect("plan");
    let value = |n: i64, at| TemporalValue { value: RuntimeValue::from_json_text(n.to_string()).expect("number"), valid_at: at, recorded_at: at, source: format!("synthetic:{n}:{at}") };
    let rows = [2, 6, 7, 9, 12, 12].into_iter().enumerate().map(|(i,y)| TemporalRow {
        key: format!("row-{i}"), at: 100, features: vec![value(i64::try_from(i).expect("bounded index") + 1, 100)], outcome: value(y, 200),
    }).collect();
    let table = plan.materialize(200, 1, rows).expect("table");
    let report = ols(&table, 0.05).expect("OLS");
    assert_eq!(report.n, 6);
    assert!((report.intercept - 1.0).abs() < 1e-12);
    assert!((report.slope - 2.0).abs() < 1e-12);
    assert!((report.slope_standard_error - (2.0_f64 / 35.0).sqrt()).abs() < 1e-12);
    assert_eq!(report.plots.residuals.len(), 6);
    assert_eq!(report.plots.qq.len(), 6);
    assert_eq!(report.plots.cooks_distance.len(), 6);
    assert!(report.interval.0 < 2.0 && report.interval.1 > 2.0);
    assert!(report.diagnostics.iter().any(|d| d.method == "sample_size_min_20" && !d.passed));
    for (actual, expected) in report.plots.residuals.iter().zip([-1.0,1.0,0.0,0.0,1.0,-1.0]) { assert!((actual.1-expected).abs() < 1e-12); }
    for alpha in [0.0, 1.0, f64::NAN] { assert!(ols(&table, alpha).is_err()); }
    let mut constant = table.rows().to_vec();
    for row in &mut constant { row.features = vec![value(1, 100)]; }
    assert!(ols(&plan.materialize(200, 1, constant).expect("constant predictor"), 0.05).is_err());
    let mut perfect = table.rows().to_vec();
    for (i,row) in perfect.iter_mut().enumerate() { row.outcome = value(i64::try_from(i).expect("index") + 1, 200); }
    assert!(ols(&plan.materialize(200, 1, perfect).expect("perfect line"), 0.05).is_err());
}
