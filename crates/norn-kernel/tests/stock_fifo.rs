//! FIFO fold/replay correspondence with the independent valuation reference.

use norn_kernel::stock_fifo::{
    FifoError, FifoLayer, FifoMovement, MAX_RECEIPT_QUANTITY, allocate_fifo, replay_fifo,
};
use serde_json::Value;
use std::error::Error;

type Checked<T> = Result<T, Box<dyn Error>>;

fn field<'a>(value: &'a Value, key: &str) -> Checked<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| "missing reference field".into())
}

fn text<'a>(value: &'a Value, key: &str) -> Checked<&'a str> {
    field(value, key)?
        .as_str()
        .ok_or_else(|| "expected reference text".into())
}

fn integer(value: &Value, key: &str) -> Checked<u128> {
    Ok(text(value, key)?.parse()?)
}

fn array(value: &Value) -> Checked<&Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| "expected reference array".into())
}

fn layers(value: &Value) -> Checked<Vec<FifoLayer>> {
    array(value)?
        .iter()
        .map(|layer| {
            Ok(FifoLayer {
                receipt: text(layer, "receipt")?.into(),
                quantity: integer(layer, "quantity")?,
                value: integer(layer, "value")?,
            })
        })
        .collect()
}

#[test]
fn fifo_fold_and_replay_match_independent_interval_reference() -> Checked<()> {
    let cases: Value = serde_json::from_str(include_str!("fixtures/stock-fifo-reference.json"))?;
    for case in array(&cases)? {
        let history: Vec<_> = array(field(case, "history")?)?
            .iter()
            .map(|event| match text(event, "kind")? {
                "receipt" => Ok(FifoMovement::Receipt(FifoLayer {
                    receipt: text(event, "id")?.into(),
                    quantity: integer(event, "quantity")?,
                    value: integer(event, "value")?,
                })),
                "delivery" => Ok(FifoMovement::Delivery(integer(event, "quantity")?)),
                _ => Err("unknown reference movement".into()),
            })
            .collect::<Checked<_>>()?;
        let mut state = Vec::new();
        let mut allocations = Vec::new();
        for event in &history {
            let result = allocate_fifo(&state, event)?;
            state = result.layers;
            allocations = result.allocations;
        }
        assert_eq!(state, layers(field(case, "layers")?)?);
        assert_eq!(allocations, layers(field(case, "allocations")?)?);
        assert_eq!(replay_fifo(&history)?, state);
    }
    Ok(())
}

#[test]
fn proportional_allocation_avoids_intermediate_overflow_and_drains_residual() -> Checked<()> {
    let layer = FifoLayer {
        receipt: "large".into(),
        quantity: MAX_RECEIPT_QUANTITY,
        value: u128::MAX,
    };
    let quantity = layer.quantity.checked_sub(1).ok_or("quantity overflow")?;
    let first = allocate_fifo(
        std::slice::from_ref(&layer),
        &FifoMovement::Delivery(quantity),
    )?;
    let final_issue = allocate_fifo(&first.layers, &FifoMovement::Delivery(1))?;
    assert!(final_issue.layers.is_empty());
    let first_value = first
        .allocations
        .first()
        .ok_or("missing first allocation")?
        .value;
    let last_value = final_issue
        .allocations
        .first()
        .ok_or("missing final allocation")?
        .value;
    assert_eq!(first_value.checked_add(last_value), Some(layer.value));
    Ok(())
}

#[test]
fn refused_oversell_and_duplicate_receipt_preserve_input() -> Checked<()> {
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
    let original = state.first().ok_or("missing original receipt")?.clone();
    assert_eq!(
        allocate_fifo(&state, &FifoMovement::Receipt(original)),
        Err(FifoError::Invalid)
    );
    assert_eq!(state, before);
    Ok(())
}
