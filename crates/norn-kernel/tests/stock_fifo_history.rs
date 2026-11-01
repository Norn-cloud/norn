//! FIFO provenance dependency and layer restoration specification.
use norn_kernel::{
    stock_fifo::{FifoLayer, FifoMovement},
    stock_fifo_history::FifoState,
    stock_history::{StockHistory, StockHistoryError, StockOperation},
};
use std::error::Error;
type Checked<T> = Result<T, Box<dyn Error>>;
type Book = StockHistory<FifoState>;
fn receipt(book: &Book, id: &str, quantity: u128, value: u128) -> Result<Book, StockHistoryError> {
    book.post(
        id.into(),
        StockOperation::Movement(
            "a".into(),
            FifoMovement::Receipt(FifoLayer {
                receipt: id.into(),
                quantity,
                value,
            }),
        ),
    )
}
#[test]
fn identical_cost_is_still_a_layer_provenance_dependency() -> Checked<()> {
    let a = receipt(&Book::default(), "a", 1, 1)?;
    let b = receipt(&a, "b", 1, 1)?;
    let issued = b.post(
        "d".into(),
        StockOperation::Movement("a".into(), FifoMovement::Delivery(1)),
    )?;
    assert!(matches!(
        issued.cancel("ca".into(), "a"),
        Err(StockHistoryError::Dependents)
    ));
    let restored = issued.cancel("cd".into(), "d")?;
    assert_eq!(restored.positions()?, b.positions()?);
    let independent = b.cancel("ca".into(), "a")?;
    assert_eq!(
        independent.positions()?,
        receipt(&Book::default(), "b", 1, 1)?.positions()?
    );
    Ok(())
}
#[test]
fn cross_layer_transfer_conserves_exact_value_and_restores_after_dependents_reverse() -> Checked<()>
{
    for first_value in 0..=6 {
        for second_value in 0..=6 {
            for quantity in 1..=5 {
                let a = receipt(&Book::default(), "a", 2, first_value)?;
                let before = receipt(&a, "b", 3, second_value)?;
                let transferred = before.post(
                    "t".into(),
                    StockOperation::Transfer("a".into(), "b".into(), quantity),
                )?;
                let event = transferred.events().last().ok_or("missing transfer")?;
                let mut changes = event.changes.iter();
                let out = changes.next().ok_or("missing source")?;
                let incoming = changes.next().ok_or("missing destination")?;
                assert_eq!(out.effect.amount, incoming.effect.amount);
                assert_eq!(out.effect.amount.quantity, quantity);
                assert!(!out.effect.allocations.is_empty());
                let issued = transferred.post(
                    "d".into(),
                    StockOperation::Movement("b".into(), FifoMovement::Delivery(1)),
                )?;
                assert!(matches!(
                    issued.cancel("ct".into(), "t"),
                    Err(StockHistoryError::Dependents)
                ));
                let reversed = issued.cancel("cd".into(), "d")?.cancel("ct".into(), "t")?;
                assert_eq!(reversed.positions()?, before.positions()?);
                let inverse = reversed.events().last().ok_or("missing inverse")?;
                for (original, opposite) in event.changes.iter().zip(&inverse.changes) {
                    assert_eq!(original.effect.amount, opposite.effect.amount);
                    assert_eq!(original.effect.allocations, opposite.effect.allocations);
                    assert_ne!(original.effect.incoming, opposite.effect.incoming);
                }
            }
        }
    }
    Ok(())
}
