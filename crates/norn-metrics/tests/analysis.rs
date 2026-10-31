//! Welch parity against a closed-form df=4 distribution and refusal boundaries.
#![allow(clippy::expect_used, clippy::arithmetic_side_effects, reason = "numerical test oracle")]
use std::path::PathBuf;
use norn_checker::EffectiveCatalog;
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use norn_metrics::analysis::welch;
use norn_metrics::temporal::{TemporalPlan, TemporalRow, TemporalValue};

#[test]
fn welch_matches_closed_form_and_refuses_invalid_groups() {
    let root = PathBuf::from(std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("prepare typed fixtures"));
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_slice(&std::fs::read(root.join("metric-demand.json")).expect("fixture")).expect("artifacts");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let plan = TemporalPlan::compile(&catalog, "demo.Demand", &["stock".into()], "units", 0).expect("plan");
    let value = |n: i64, at| TemporalValue { value: RuntimeValue::from_json_text(&n.to_string()).expect("number"), valid_at: at, recorded_at: at, source: format!("synthetic:{n}:{at}") };
    let rows = || (1..=6).map(|n| TemporalRow { key: format!("synthetic-{n}"), at: 100, features: vec![value(i64::from(n > 3), 100)], outcome: value(n, 200) }).collect::<Vec<_>>();
    let table = plan.materialize(200, 1, rows()).expect("table");
    let report = welch(&table, 0.05).expect("report");
    assert_eq!(report.n, (3, 3));
    assert_eq!(report.difference, -3.0);
    assert!((report.statistic + 3.0 * 1.5_f64.sqrt()).abs() < 1e-12);
    assert!((report.degrees_of_freedom - 4.0).abs() < 1e-12);
    // Integrating the df=4 density gives F(t)=1/2+3x/4-x^3/4,
    // where x=t/sqrt(t^2+4). This oracle does not call statrs.
    let x = (27.0_f64 / 35.0).sqrt();
    let expected_p = 1.0 - 1.5 * x + 0.5 * x.powi(3);
    assert!((report.p_value - expected_p).abs() < 1e-12);
    assert!(report.interval.0 < -3.0 && report.interval.1 > -3.0 && report.interval.1 < 0.0);
    assert!(!report.sample_size_passed, "three rows do not pass the conservative 30/group diagnostic");
    for alpha in [0.0, 1.0, f64::NAN, f64::INFINITY] { assert!(welch(&table, alpha).is_err()); }
    let mut invalid = rows();
    invalid.first_mut().expect("row").features.first_mut().expect("feature").value = RuntimeValue::from_json_text("2").expect("number");
    assert!(welch(&plan.materialize(200, 1, invalid).expect("numeric table"), 0.05).is_err());
    let mut constant = rows();
    for row in &mut constant { row.outcome = value(1, 200); }
    assert!(welch(&plan.materialize(200, 1, constant).expect("table"), 0.05).is_err());
    assert!(welch(&plan.materialize(200, 1, rows().into_iter().take(3).collect()).expect("one group"), 0.05).is_err());
}
