//! Closed controls wire contract and independent evidence axes.
use norn_ir::ObjectTypeDef;
use norn_ir::entity_v2::controls::{ControlStatus, ControlVerdict, Reachability};
use serde_json::json;

#[test]
fn controls_are_optional_but_never_null() -> Result<(), serde_json::Error> {
    let omitted: ObjectTypeDef = serde_json::from_value(json!({"name": "demo.Order"}))?;
    assert!(omitted.controls.is_none());
    assert!(
        serde_json::from_value::<ObjectTypeDef>(json!({
            "name": "demo.Order", "controls": null
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ObjectTypeDef>(json!({
            "name": "demo.Order", "controls": {"format": "norn.controls/2", "sensitivePaths": []}
        }))
        .is_err()
    );
    Ok(())
}

#[test]
fn verdict_wire_preserves_both_evidence_axes() -> Result<(), serde_json::Error> {
    for verdict in [
        ControlStatus::Proven,
        ControlStatus::Refuted,
        ControlStatus::Unknown,
    ] {
        for reachability in [
            Reachability::Witnessed,
            Reachability::Unknown,
            Reachability::Unreachable,
        ] {
            let value = ControlVerdict {
                object_type: norn_ir::ObjectTypeName("demo.Order".into()),
                actions: vec!["demo.approve".into()],
                verdict: verdict.clone(),
                reachability,
                trace: vec![],
                explanation: "finite model".into(),
            };
            assert_eq!(
                serde_json::from_value::<ControlVerdict>(serde_json::to_value(&value)?)?,
                value
            );
        }
    }
    Ok(())
}
