//! Transfer conservation and dependency-aware exact inverse specification.
use norn_kernel::{
    stock_average::{AverageMovement, AverageState, StockAmount},
    stock_history::{StockHistory, StockHistoryError, StockOperation},
};
use std::error::Error;
type Checked<T> = Result<T, Box<dyn Error>>;
type Book = StockHistory<AverageState>;
fn receipt(
    book: &Book,
    id: &str,
    partition: &str,
    quantity: u128,
    value: u128,
) -> Result<Book, StockHistoryError> {
    book.post(
        id.into(),
        StockOperation::Movement(
            partition.into(),
            AverageMovement::Receipt(StockAmount { quantity, value }),
        ),
    )
}
fn delivery(
    book: &Book,
    id: &str,
    partition: &str,
    quantity: u128,
) -> Result<Book, StockHistoryError> {
    book.post(
        id.into(),
        StockOperation::Movement(
            partition.into(),
            AverageMovement::Delivery(id.into(), quantity),
        ),
    )
}
#[test]
fn transfer_carries_exact_source_cost_without_profit_and_refusal_is_atomic() -> Checked<()> {
    for quantity in 1..=6 {
        for value in 0..=6 {
            for moved in 1..=quantity {
                let before = receipt(&Book::default(), "r", "a", quantity, value)?;
                let after = before.post(
                    "t".into(),
                    StockOperation::Transfer("a".into(), "b".into(), moved),
                )?;
                let positions = after.positions()?;
                let source = positions.get("a").ok_or("missing source")?.balance();
                let destination = positions.get("b").ok_or("missing destination")?.balance();
                assert_eq!(
                    source.quantity.checked_add(destination.quantity),
                    Some(quantity)
                );
                assert_eq!(source.value.checked_add(destination.value), Some(value));
                let event = after.events().last().ok_or("missing transfer")?;
                let reversed = after.cancel("c".into(), "t")?;
                assert_eq!(reversed.positions()?, before.positions()?);
                let inverse = reversed.events().last().ok_or("missing inverse")?;
                for (original, opposite) in event.changes.iter().zip(&inverse.changes) {
                    assert_eq!(original.partition, opposite.partition);
                    assert_eq!(original.effect.amount, opposite.effect.amount);
                    assert_ne!(original.effect.incoming, opposite.effect.incoming);
                }
                assert!(reversed.cancel("again".into(), "t").is_err());
            }
        }
    }
    let before = receipt(&Book::default(), "full", "b", 1, u128::MAX)?;
    let before = receipt(&before, "r", "a", 1, 1)?;
    let positions = before.positions()?;
    assert!(
        before
            .post(
                "t".into(),
                StockOperation::Transfer("a".into(), "b".into(), 1)
            )
            .is_err()
    );
    assert_eq!(before.positions()?, positions);
    assert_eq!(before.events().len(), 2);
    Ok(())
}
#[test]
fn dependent_issues_and_returns_reverse_before_source_restoration() -> Checked<()> {
    let before = receipt(&Book::default(), "r", "a", 5, 3)?;
    let transfer = before.post(
        "t".into(),
        StockOperation::Transfer("a".into(), "b".into(), 5),
    )?;
    let issued = delivery(&transfer, "d", "b", 3)?;
    let returned = issued.post(
        "ret".into(),
        StockOperation::Movement("b".into(), AverageMovement::Return("d".into(), 1)),
    )?;
    assert!(matches!(
        returned.cancel("ct".into(), "t"),
        Err(StockHistoryError::Dependents)
    ));
    assert!(matches!(
        returned.cancel("cd".into(), "d"),
        Err(StockHistoryError::Dependents)
    ));
    let restored = returned
        .cancel("cr".into(), "ret")?
        .cancel("cd".into(), "d")?
        .cancel("ct".into(), "t")?;
    assert_eq!(restored.positions()?, before.positions()?);
    assert_eq!(returned.events().len(), 4);
    Ok(())
}
