use norn_kernel::stock_fifo::{
    FifoError, FifoLayer, FifoMovement, MAX_RECEIPT_QUANTITY, allocate_fifo, replay_fifo,
};
use serde_json::Value;

fn integer(value: &Value, key: &str) -> u128 {
    value[key].as_str().unwrap().parse().unwrap()
}

fn layers(value: &Value) -> Vec<FifoLayer> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|layer| FifoLayer {
            receipt: layer["receipt"].as_str().unwrap().into(),
            quantity: integer(layer, "quantity"),
            value: integer(layer, "value"),
        })
        .collect()
}

#[test]
fn fifo_fold_and_replay_match_independent_interval_reference() {
    let cases: Value =
        serde_json::from_str(include_str!("fixtures/stock-fifo-reference.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let history: Vec<_> = case["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| match event["kind"].as_str().unwrap() {
                "receipt" => FifoMovement::Receipt(FifoLayer {
                    receipt: event["id"].as_str().unwrap().into(),
                    quantity: integer(event, "quantity"),
                    value: integer(event, "value"),
                }),
                "delivery" => FifoMovement::Delivery(integer(event, "quantity")),
                other => panic!("unknown reference movement: {other}"),
            })
            .collect();
        let mut state = Vec::new();
        let mut allocations = Vec::new();
        for event in &history {
            let result = allocate_fifo(&state, event).unwrap();
            state = result.layers;
            allocations = result.allocations;
        }
        assert_eq!(state, layers(&case["layers"]));
        assert_eq!(allocations, layers(&case["allocations"]));
        assert_eq!(replay_fifo(&history).unwrap(), state);
    }
}

#[test]
fn proportional_allocation_avoids_intermediate_overflow_and_drains_residual() {
    let layer = FifoLayer {
        receipt: "large".into(),
        quantity: MAX_RECEIPT_QUANTITY,
        value: u128::MAX,
    };
    let first = allocate_fifo(
        &[layer.clone()],
        &FifoMovement::Delivery(layer.quantity - 1),
    )
    .unwrap();
    let final_issue = allocate_fifo(&first.layers, &FifoMovement::Delivery(1)).unwrap();
    assert!(final_issue.layers.is_empty());
    assert_eq!(
        first.allocations[0].value + final_issue.allocations[0].value,
        layer.value
    );
}

#[test]
fn refused_oversell_and_duplicate_receipt_preserve_input() {
    let state = vec![FifoLayer {
        receipt: "a".into(),
        quantity: 2,
        value: 1,
    }];
    let before = state.clone();
    assert_eq!(
        allocate_fifo(&state, &FifoMovement::Delivery(3)),
        Err(FifoError::InsufficientStock)
    );
    assert_eq!(
        allocate_fifo(&state, &FifoMovement::Delivery(0)),
        Err(FifoError::Invalid)
    );
    assert_eq!(
        allocate_fifo(&state, &FifoMovement::Receipt(state[0].clone())),
        Err(FifoError::Invalid)
    );
    assert_eq!(state, before);
}
