//! Correspondence with independent average/return specification at every prefix.
use norn_kernel::stock_average::{
    AverageError, AverageMovement, AverageState, StockAmount, allocate_average, replay_average,
};
use serde_json::Value;
use std::error::Error;
type Checked<T> = Result<T, Box<dyn Error>>;
fn text<'a>(value: &'a Value, key: &str) -> Checked<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| "missing text".into())
}
fn amount(value: &Value) -> Checked<StockAmount> {
    Ok(StockAmount {
        quantity: text(value, "quantity")?.parse()?,
        value: text(value, "value")?.parse()?,
    })
}
#[test]
fn every_prefix_matches_reference_including_repricing_residual_and_capped_returns() -> Checked<()> {
    let cases: Vec<Vec<Value>> =
        serde_json::from_str(include_str!("fixtures/stock-average-reference.json"))?;
    assert_eq!(cases.len(), 648);
    for history in cases {
        let mut state = AverageState::default();
        let mut movements = Vec::new();
        for step in history {
            let event = step.get("event").ok_or("missing event")?;
            let quantity = text(event, "quantity")?.parse()?;
            let movement = match text(event, "kind")? {
                "receipt" => AverageMovement::Receipt(amount(event)?),
                "delivery" => AverageMovement::Delivery(text(event, "id")?.into(), quantity),
                "return" => AverageMovement::Return(text(event, "delivery")?.into(), quantity),
                _ => return Err("unknown movement".into()),
            };
            let result = allocate_average(&state, &movement)?;
            assert_eq!(
                result.allocation,
                amount(step.get("allocation").ok_or("missing allocation")?)?
            );
            assert_eq!(
                result.state.balance(),
                amount(step.get("balance").ok_or("missing balance")?)?
            );
            state = result.state;
            movements.push(movement);
            assert_eq!(replay_average(&movements)?, state);
        }
    }
    Ok(())
}
#[test]
fn large_value_and_refusals_preserve_input() -> Checked<()> {
    let initial = AverageMovement::Receipt(StockAmount {
        quantity: 5,
        value: u128::MAX,
    });
    let state = replay_average(&[initial])?;
    let before = state.clone();
    for event in [
        AverageMovement::Delivery("x".into(), 6),
        AverageMovement::Return("missing".into(), 1),
        AverageMovement::Receipt(StockAmount {
            quantity: 1,
            value: 1,
        }),
    ] {
        assert!(allocate_average(&state, &event).is_err());
        assert_eq!(state, before);
    }
    let first = allocate_average(&state, &AverageMovement::Delivery("x".into(), 4))?;
    let last = allocate_average(&first.state, &AverageMovement::Delivery("y".into(), 1))?;
    assert_eq!(
        first.allocation.value.checked_add(last.allocation.value),
        Some(u128::MAX)
    );
    assert_eq!(last.state.balance(), StockAmount::default());
    assert_eq!(
        allocate_average(&first.state, &AverageMovement::Return("x".into(), 5)),
        Err(AverageError::OverReturn)
    );
    assert_eq!(
        allocate_average(&first.state, &AverageMovement::Delivery("x".into(), 1)),
        Err(AverageError::Invalid)
    );
    assert_eq!(
        allocate_average(&first.state, &AverageMovement::Return("x".into(), 0)),
        Err(AverageError::Invalid)
    );
    Ok(())
}
