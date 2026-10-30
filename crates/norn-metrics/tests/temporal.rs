//! Synthetic typed authoring through the production ontology and temporal checks.
#![allow(clippy::expect_used, reason = "test failures identify their assertion")]

use std::path::PathBuf;

use norn_checker::EffectiveCatalog;
use norn_ir::{ArtifactEnvelope, RuntimeValue};
use norn_metrics::temporal::{TemporalError, TemporalPlan, TemporalRow, TemporalValue};

#[test]
fn admits_only_point_in_time_rows_and_chronological_holdouts() {
    let root = PathBuf::from(
        std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR")
            .expect("generate typed fixtures with scripts/ci/core.sh before testing"),
    );
    let bytes =
        std::fs::read(root.join("metric-demand.json")).expect("generated synthetic fixture");
    let artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_slice(&bytes).expect("generated artifacts");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let plan = TemporalPlan::compile(&catalog, "demo.Demand", &["stock".into()], "units", 10)
        .expect("plan");
    let value = |at| TemporalValue {
        value: RuntimeValue::from_json_text("12").expect("exact integer"),
        valid_at: at,
        recorded_at: at,
        source: format!("synthetic:{at}"),
    };
    let row = |at, label| TemporalRow {
        key: "fictional-sku".into(),
        at,
        features: vec![value(at)],
        outcome: value(label),
    };
    let table = plan
        .materialize(300, 50, vec![row(100, 103), row(200, 203)])
        .expect("table");
    let (train, test) = table.split(150).expect("holdout");
    assert_eq!((train.len(), test.len()), (1, 1));
    assert!(table.split(103).is_err(), "boundary label is unavailable");
    let mut correction = row(100, 103);
    correction.outcome.recorded_at = 160;
    let corrected = plan
        .materialize(300, 50, vec![correction, row(200, 203)])
        .expect("late correction is known at extraction");
    assert!(
        corrected.split(150).is_err(),
        "training cannot use a backdated correction learned after the boundary"
    );
    assert_eq!(
        (table.as_of(), table.source_head(), table.rows().len()),
        (300, 50, 2)
    );
    assert_eq!(table.plan().object_type(), "demo.Demand");
    let mut late = row(100, 103);
    late.features.first_mut().expect("feature").recorded_at = 101;
    assert_eq!(
        plan.materialize(300, 50, vec![late])
            .expect_err("future knowledge"),
        TemporalError::Row("feature leakage")
    );
    let mut stale = row(100, 103);
    stale.features.first_mut().expect("feature").valid_at = 89;
    assert_eq!(
        plan.materialize(300, 50, vec![stale]).expect_err("stale"),
        TemporalError::Row("stale feature")
    );
    assert!(plan.materialize(102, 50, vec![row(100, 103)]).is_err());
    assert!(plan.materialize(300, 0, vec![row(100, 103)]).is_err());
    assert!(
        plan.materialize(300, 50, vec![row(100, 103), row(100, 103)])
            .is_err()
    );
    for feature in ["id", "units", "absent"] {
        assert!(
            TemporalPlan::compile(&catalog, "demo.Demand", &[feature.into()], "units", 10).is_err()
        );
    }
    let unit = TemporalPlan::compile(&catalog, "demo.Demand", &["weight".into()], "units", 10)
        .expect("unit plan");
    assert_eq!(
        unit.features()
            .first()
            .expect("column")
            .unit
            .as_ref()
            .expect("unit")
            .code,
        "kg"
    );
}
