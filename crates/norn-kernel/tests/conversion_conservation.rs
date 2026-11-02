#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::pedantic,
    reason = "named core conversion conformance scenarios"
)]
//! VAL-DECIMAL-SCALE-1 and LAW-CONVERSION-1: the shipped core invariant governs every Melt write.
use norn_ir::{EntityInterfaceRootV2, InterfaceDef, ObjectTypeDef};
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
    let interface = core_interface();
    let mut properties = vec![json!({"id":"batch", "type":{"kind":"text"}})];
    properties.extend(["input_quantity", "output_quantity", "loss_quantity"].map(|id|
        json!({"id":id,"type":{"kind":"decimal","scale":3,"unit":{"system":"ucum","code":"g"}}})));
    let melt: ObjectTypeDef = serde_json::from_value(json!({
        "name":"jewelry.Melt", "role":"happening", "implements":["core.Conversion"],
        "properties": properties
    }))
    .expect("Melt declares required properties, never a conservation expression");
    MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(vec![melt])
        .with_interfaces(vec![
            interface,
            serde_json::from_str::<norn_ir::ArtifactEnvelope>(include_str!(
                "../../../registry/core/artifacts/core.Process.interface.json"
            ))
            .and_then(|a| {
                serde_json::from_value::<EntityInterfaceRootV2>(serde_json::to_value(
                    a.payload.to_json_value(),
                )?)
            })
            .expect("generated Process interface")
            .interface,
        ])
}

fn measurement(id: &str, input: &str, output: &str, loss: &str) -> FactWrite {
    let values = format!(
        r#"{{"batch":"{id}","input_quantity":{input},"output_quantity":{output},"loss_quantity":{loss}}}"#
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
    for (output, loss) in [("99.2", "0"), ("100.1", "-0.1"), ("99.1999", "0.8001")] {
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
        let refusal = result.expect_err("invalid measurements refuse");
        let norn_kernel::cell_host::CommitError::InvariantViolation { id, .. } = refusal else {
            panic!("expected an integrity invariant refusal");
        };
        if loss == "0.8001" {
            assert!(matches!(
                id.as_str(),
                "$property.output_quantity" | "$property.loss_quantity"
            ));
        } else {
            assert_eq!(id.as_str(), "conversion_conservation");
        }

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

#[test]
fn law_conversion_missing_loss_refuses_without_partial_writes() {
    let mut invalid = measurement("missing", "100", "99.2", "0.8");
    let FactMutation::Put { values } = &mut invalid.mutation else {
        panic!("put")
    };
    values.remove("loss_quantity");
    let mut cell = cell();
    assert!(
        cell.commit(CellCommit {
            writes: vec![measurement("balanced", "100", "99.2", "0.8"), invalid],
            events: vec!["measured".into()],
            effects: vec![notification()],
            ..CellCommit::empty()
        })
        .is_err()
    );
    for id in ["balanced", "missing"] {
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

proptest! {
    #![proptest_config(norn_testkit::Settings::new(file!()).config())]
    #[test]
    fn decimal_scale_generated_balanced_measurements_refuse_unrepresentable_fractions(quanta in 1_u32..10_000) {
        let output = format!("0.{quanta:04}");
        let remainder = 10_000_u32.checked_sub(quanta).expect("bounded");
        let loss = format!("0.{remainder:04}");
        let mut cell = cell();
        let result = cell.commit(CellCommit {
            writes: vec![measurement("scaled", "1", &output, &loss)],
            ..CellCommit::empty()
        });
        prop_assert_eq!(result.is_ok(), quanta.is_multiple_of(10));
    }
}

#[test]
fn decimal_scale_record_members_refuse_the_whole_commit() {
    let definition: ObjectTypeDef = serde_json::from_value(json!({"name":"toy.Reading","properties":[
        {"id":"reading","type":{"kind":"record","members":[{"id":"amount","type":{"kind":"decimal","scale":3}}]}}
    ]})).expect("record type");
    let write = |id: &str, amount: &str| FactWrite {
        key: FactKey::new("toy.Reading", id),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: serde_json::from_str(&format!(r#"{{"reading":{{"amount":{amount}}}}}"#))
                .expect("exact record"),
        },
    };
    for (amount, allowed) in [("1.234", true), ("1.2345", false)] {
        let mut cell: Cell =
            MemoryCell::new(|command: &Command<JsonObject>| command.payload.clone())
                .with_types(vec![definition.clone()]);
        let result = cell.commit(CellCommit {
            writes: vec![write("balanced", "1.234"), write("candidate", amount)],
            events: vec!["reading".into()],
            effects: vec![notification()],
            ..CellCommit::empty()
        });
        assert_eq!(result.is_ok(), allowed);
        if !allowed {
            assert!(
                matches!(result, Err(norn_kernel::cell_host::CommitError::InvariantViolation { ref id, .. }) if id.as_str() == "$property.reading")
            );
            for id in ["balanced", "candidate"] {
                assert!(
                    !cell
                        .load(&FactKey::new("toy.Reading", id))
                        .expect("read")
                        .exists()
                );
            }
            assert!(cell.ledger().is_empty());
            assert!(cell.outbox().is_empty());
        }
    }
}
