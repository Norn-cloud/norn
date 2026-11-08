#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

//! A catalog beyond the inheritance classification budget is refused by
//! `Model::new`; there is no model whose queries could report a registered
//! type as unknown.

use norn_cell::{CellError, Model};
use norn_ir::ObjectTypeDef;
use norn_projections::SqliteDdlInput;

fn types(count: usize) -> Vec<ObjectTypeDef> {
    (0..count)
        .map(|index| {
            serde_json::from_value(serde_json::json!({ "name": format!("budget.T{index:04}") }))
                .expect("object type")
        })
        .collect()
}

#[test]
fn more_than_4096_types_make_model_new_refuse() {
    let objects = types(4_097);
    let refusal = Model::new(&SqliteDdlInput {
        object_types: &objects,
        property_types: &[],
    })
    .expect_err("an unclassifiable catalog must not become a model");
    match refusal {
        CellError::EffectiveViewRefused(refusal) => {
            assert_eq!((refusal.dimension, refusal.limit), ("types", 4_096));
        }
        other => panic!("expected the effective-view refusal, got {other:?}"),
    }
}

#[test]
fn exactly_4096_types_still_build_a_model_that_knows_every_type() {
    let objects = types(4_096);
    let model = Model::new(&SqliteDdlInput {
        object_types: &objects,
        property_types: &[],
    })
    .expect("4,096 types classify within budget");
    assert!(model.has_type("budget.T0000"));
    assert!(model.has_type("budget.T4095"));
}
