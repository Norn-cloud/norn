#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::pedantic,
    reason = "named core conversion conformance scenarios"
)]
//! LAW-CONVERSION-1: the shipped core invariant governs every Melt write.
use norn_ir::{EntityInterfaceRootV2, InterfaceDef, ObjectTypeDef, PropertyTypeCatalogRootV2};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, Effect, FactKey, FactMutation, FactVersion,
    FactWrite, FreshnessPolicy, Generation, IdempotencyKey,
};
use norn_testkit::cell_host::MemoryCell;
use norn_testkit::proptest::prelude::*;
use serde_json::json;

type Cell = MemoryCell<String, JsonObject, JsonObject>;

fn core_interface() -> InterfaceDef {
    let artifact: serde_json::Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.Conversion.interface.json"
    ))
    .expect("generated core interface");
    serde_json::from_value::<EntityInterfaceRootV2>(artifact["payload"].clone())
        .expect("interface root")
        .interface
}

fn cell() -> Cell {
    let artifact: serde_json::Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/std.value_types.entity.json"
    ))
    .expect("shared core types");
    let property_types: PropertyTypeCatalogRootV2 =
        serde_json::from_value(artifact["payload"].clone()).expect("property types");
    let interface = core_interface();
    let mut properties = serde_json::to_value(&interface.properties).expect("required members");
    properties
        .as_array_mut()
        .expect("properties")
        .push(json!({"id":"batch", "type":{"kind":"text"}}));
    let melt: ObjectTypeDef = serde_json::from_value(json!({
        "name":"jewelry.Melt", "implements":["core.Conversion"],
        "properties": properties
    }))
    .expect("Melt declares required properties, never a conservation expression");
    MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(vec![melt])
        .with_interfaces(vec![interface])
        .with_property_types(property_types.property_types)
}

fn measurement(id: &str, input: &str, output: &str, loss: &str) -> FactWrite {
    let values = format!(
        r#"{{"batch":"{id}","input_quantity":{input},"output_quantity":{output},"loss_quantity":{loss},"quantity_unit":"g"}}"#
    );
    FactWrite {
        key: FactKey::new("jewelry.Melt", id),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: serde_json::from_str(&values).expect("exact runtime decimals"),
        },
    }
}

fn notification() -> Effect<JsonObject, JsonObject> {
    Effect::Perform(Command {
        id: CommandId("publish".into()),
        generation: Generation(0),
        idempotency_key: IdempotencyKey("publish".into()),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: None,
        payload: JsonObject::new(),
    })
}

#[test]
fn law_conversion_balanced_measurements_inherit_the_core_invariant() {
    let mut cell = cell();
    cell.commit(CellCommit {
        writes: vec![measurement("melt", "100", "99.2", "0.8")],
        ..CellCommit::empty()
    })
    .expect("100 = 99.2 + explicitly recorded 0.8");
    assert!(
        cell.load(&FactKey::new("jewelry.Melt", "melt"))
            .expect("read")
            .exists()
    );
    assert_eq!(cell.ledger().len(), 1);
}

#[test]
fn law_counterexample_omitted_loss_refuses_the_whole_commit_and_outbox() {
    for (output, loss) in [("99.2", "0"), ("100.1", "-0.1"), ("99.2", "0.8001")] {
        let mut cell = cell();
        let result = cell.commit(CellCommit {
            writes: vec![
                measurement("balanced", "100", "99.2", "0.8"),
                measurement("invalid", "100", output, loss),
            ],
            events: vec!["conversion measured".into()],
            effects: vec![notification()],
            ..CellCommit::empty()
        });
        assert!(
            result.is_err(),
            "invalid output/loss must refuse: {output}/{loss}"
        );
        for id in ["balanced", "invalid"] {
            assert!(
                !cell
                    .load(&FactKey::new("jewelry.Melt", id))
                    .expect("read")
                    .exists()
            );
        }
        assert!(cell.ledger().is_empty());
        assert!(cell.outbox().is_empty());
    }
}

proptest! {
    #![proptest_config(norn_testkit::Settings::new(file!()).config())]
    #[test]
    fn law_conversion_generated_measurements_match_exact_integer_oracle(
        output in 0_u32..10_000, loss in 0_u32..10_000, excess in 0_u32..2
    ) {
        let input = output.checked_add(loss).and_then(|sum| sum.checked_add(excess)).expect("bounded sum");
        let mut cell = cell();
        let result = cell.commit(CellCommit {
            writes: vec![measurement("generated", &input.to_string(), &output.to_string(), &loss.to_string())],
            ..CellCommit::empty()
        });
        prop_assert_eq!(result.is_ok(), output.checked_add(loss) == Some(input));
        prop_assert_eq!(cell.ledger().is_empty(), result.is_err());
    }
}
