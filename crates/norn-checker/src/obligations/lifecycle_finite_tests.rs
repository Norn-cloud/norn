//! Bounded lifecycle input snapshots; no backend verdict is simulated.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded source fixtures"
)]
use super::lifecycle::finite::{Batch, Bounds};
use super::*;

fn source(entity_root: bool) -> DefinitionSource {
    let mut declarations: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/jewelry_demo.json"))
            .expect("actual authored source");
    if entity_root {
        // A separate supported test declaration; the actual App-root pack stays Unknown.
        declarations[1]["payload"]["scope"]["root"] = json!({"kind":"entity","id":"demo.Melt"});
        for artifact in declarations.as_array_mut().expect("artifacts") {
            artifact["provenance"]["sourcePath"] = json!("tests/lifecycle_finite");
        }
        declarations[1]["payload"]["provenance"]["sourcePath"] = json!("tests/lifecycle_finite");
    }
    let units: Value = serde_json::from_str(include_str!(
        "../../../../registry/core/artifacts/core.units.json"
    ))
    .expect("units");
    DefinitionSource::parse(
        &serde_json::to_string(&declarations).expect("source"),
        &serde_json::to_string(&vec![units]).expect("base"),
    )
    .expect("exact source")
}
fn bounds() -> Bounds {
    Bounds {
        object_type: "demo.Melt".into(),
        property: "status".into(),
        seed: 0,
        frozen: BTreeMap::from([
            ("id".into(), vec!["\"synthetic\"".into()]),
            ("input_fine_g".into(), vec!["2".into()]),
            ("output_fine_g".into(), vec!["1".into()]),
            ("loss_fine_g".into(), vec!["0".into(), "1".into()]),
        ]),
    }
}
fn batch(source: &DefinitionSource, bounds: &Bounds) -> Batch {
    super::lifecycle_tests::authority(source)
        .lifecycle_inputs(source, &BTreeSet::new(), bounds)
        .expect("fresh challenge")
}
#[test]
fn lifecycle_inputs_are_exact_complete_and_source_bound() {
    let prepared = batch(&source(true), &bounds());
    let snapshot = prepared.snapshot.expect("separate supported declaration");
    assert_eq!(snapshot.nodes.len(), 10);
    assert_eq!(snapshot.machine.states.len(), 5);
    assert_eq!(snapshot.laws.len(), 1);
    assert!(
        matches!(&snapshot.fields["input_fine_g"],norn_ir::PropertyType::Decimal{scale,unit:Some(unit),..} if scale.0==3 && unit.code=="g")
    );
    for state in &snapshot.machine.states {
        assert_eq!(
            snapshot
                .nodes
                .iter()
                .filter(|row| row["status"] == norn_ir::RuntimeValue::String(state.clone()))
                .count(),
            2
        );
    }
    let repeated = batch(&source(true), &bounds());
    assert_eq!(prepared.challenge.sha256(), repeated.challenge.sha256());
    assert_eq!(prepared.bounds_json, repeated.bounds_json);
}
#[test]
fn lifecycle_input_refusals_never_clip_the_domain() {
    assert!(
        batch(&source(false), &bounds())
            .snapshot
            .expect_err("actual App root stays Unknown")
            .contains("UnsupportedUntilB2")
    );
    let mut profile = bounds();
    profile
        .frozen
        .insert("loss_fine_g".into(), vec!["0.0001".into()]);
    assert!(batch(&source(true), &profile).snapshot.is_err());
    profile = bounds();
    profile.frozen.remove("id");
    assert!(
        batch(&source(true), &profile)
            .snapshot
            .expect_err("no implicit context")
            .contains("missing explicit")
    );
    profile = bounds();
    profile
        .frozen
        .insert("status".into(), vec!["\"posted\"".into()]);
    assert!(
        batch(&source(true), &profile)
            .snapshot
            .expect_err("no enum clipping")
            .contains("extra frozen")
    );
    profile = bounds();
    profile
        .frozen
        .insert("id".into(), vec!["\"synthetic\"".into(); 600]);
    let reason = batch(&source(true), &profile)
        .snapshot
        .expect_err("full product refused");
    assert!(reason.contains("4096") || reason.contains("budget"));
}
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn every_frozen_choice_keeps_the_complete_chart(loss in 0_u8..5) {
        let mut profile=bounds();profile.frozen.insert("loss_fine_g".into(),vec![loss.to_string()]);
        let snapshot=batch(&source(true),&profile).snapshot.expect("bounded");
        proptest::prop_assert_eq!(snapshot.nodes.len(),snapshot.machine.states.len());
        for row in snapshot.nodes { proptest::prop_assert_eq!(&row["loss_fine_g"],&norn_ir::RuntimeValue::from_json_text(&loss.to_string()).expect("exact")); }
    }
}
