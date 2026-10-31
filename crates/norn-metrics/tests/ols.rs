//! Synthetic OLS reference with exact, orthogonal residuals.
#![allow(clippy::expect_used, clippy::arithmetic_side_effects, reason = "independent numerical regression oracle")]
use norn_checker::EffectiveCatalog;
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use norn_metrics::{analysis::ols, temporal::{TemporalPlan, TemporalRow, TemporalTable, TemporalValue}};

fn table(outcomes: &[i64], constant_predictor: bool) -> TemporalTable {
    let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("prepare typed fixtures");
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_slice(&std::fs::read(std::path::Path::new(&root).join("metric-demand.json")).expect("fixture")).expect("artifacts");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let plan = TemporalPlan::compile(&catalog, "demo.Demand", &["stock".into()], "units", 0).expect("plan");
    let value = |n: i64, at| TemporalValue { value: RuntimeValue::from_json_text(n.to_string()).expect("number"), valid_at: at, recorded_at: at, source: format!("synthetic:{n}:{at}") };
    let rows = outcomes.iter().copied().enumerate().map(|(i,y)| TemporalRow {
        key: format!("row-{i}"), at: 100,
        features: vec![value(if constant_predictor { 1 } else { i64::try_from(i).expect("bounded index") + 1 }, 100)], outcome: value(y, 200),
    }).collect();
    plan.materialize(200, 1, rows).expect("table")
}

#[test]
fn regression_matches_fit_diagnostic_and_plot_oracles() {
    let admitted = table(&[2, 6, 7, 9, 12, 12], false);
    let report = ols(&admitted, 0.05).expect("OLS");
    assert_eq!(report.n, 6);
    assert!((report.intercept - 1.0).abs() < 1e-12);
    assert!((report.slope - 2.0).abs() < 1e-12);
    assert!((report.slope_standard_error - (2.0_f64 / 35.0).sqrt()).abs() < 1e-12);
    assert_eq!(report.plots.residuals.len(), 6);
    assert_eq!(report.plots.qq.len(), 6);
    assert_eq!(report.plots.cooks_distance.len(), 6);
    assert!(report.interval.0 < 2.0 && report.interval.1 > 2.0);
    assert!(report.diagnostics.iter().any(|d| d.method == "sample_size_min_20" && !d.passed));
    let x = (35.0_f64 / 37.0).sqrt();
    assert!((report.p_value - (1.0 - 1.5*x + 0.5*x.powi(3))).abs() < 1e-12);
    let diagnostic = |name| report.diagnostics.iter().find(|d| d.method == name).expect("named diagnostic");
    assert!((diagnostic("jarque_bera").statistic - 9.0/16.0).abs() < 1e-12);
    assert!((diagnostic("jarque_bera").p_value.expect("tail") - (-9.0_f64/32.0).exp()).abs() < 1e-12);
    assert!(diagnostic("breusch_pagan_lm").statistic.abs() < 1e-12);
    assert!((diagnostic("durbin_watson_1_to_3_heuristic").statistic - 2.5).abs() < 1e-12);
    assert!((report.plots.cooks_distance.first().expect("first") - 231.0/200.0).abs() < 1e-12);
    assert!((report.plots.qq.get(1).expect("quartile").0 + 0.674_489_750_196_081_7).abs() < 1e-12);
    for (actual, expected) in report.plots.residuals.iter().zip([-1.0,1.0,0.0,0.0,1.0,-1.0]) { assert!((actual.1-expected).abs() < 1e-12); }
    for alpha in [0.0, 1.0, f64::NAN] { assert!(ols(&admitted, alpha).is_err()); }
    assert!(ols(&table(&[1, 2, 3, 4, 5, 6], true), 0.05).is_err());
    assert!(ols(&table(&[1, 2, 3, 4, 5, 6], false), 0.05).is_err());
}

#[test]
fn residuals_are_invariant_under_a_representable_large_outcome_translation() {
    let origin = 10_000_000_000_000_000_i64;
    let perfect = [0, 2, 4, 6, 8, 10].map(|y| origin + y);
    assert!(ols(&table(&perfect, false), 0.05).is_err());
    let outcomes = [4, 12, 14, 18, 24, 24];
    let baseline = ols(&table(&outcomes, false), 0.05).expect("baseline");
    let shifted = ols(&table(&outcomes.map(|y| origin + y), false), 0.05).expect("translated");
    assert!((baseline.slope - shifted.slope).abs() < 1e-12);
    assert!((baseline.slope_standard_error - shifted.slope_standard_error).abs() < 1e-12);
    assert!((baseline.p_value - shifted.p_value).abs() < 1e-12);
    for (a,b) in baseline.plots.residuals.iter().zip(&shifted.plots.residuals) { assert!((a.1-b.1).abs() < 1e-12); }
    for (a,b) in baseline.diagnostics.iter().zip(&shifted.diagnostics) { assert!((a.statistic-b.statistic).abs() < 1e-12); }
}
