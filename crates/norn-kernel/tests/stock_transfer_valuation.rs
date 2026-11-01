//! Transfers deplete stock without authorizing a customer return.
use norn_kernel::{
    stock_average::{AverageMovement, AverageState, StockAmount, allocate_average},
    stock_valuation::{StockHistoryError, Valuation},
};
use std::error::Error;
#[test]
fn transfer_issue_is_never_returnable_and_its_identity_cannot_be_reused()
-> Result<(), Box<dyn Error>> {
    for quantity in 1..=6 {
        for value in 0..=6 {
            let before = allocate_average(
                &AverageState::default(),
                &AverageMovement::Receipt(StockAmount { quantity, value }),
            )?
            .state;
            let (after, allocation) = before.transfer("t", quantity)?;
            assert_eq!(allocation.amount, before.balance());
            assert_eq!(after.balance(), StockAmount::default());
            assert!(
                allocate_average(&after, &AverageMovement::Return("t".into(), quantity)).is_err()
            );
            let refilled = allocate_average(
                &after,
                &AverageMovement::Receipt(StockAmount { quantity, value }),
            )?
            .state;
            assert!(
                allocate_average(&refilled, &AverageMovement::Delivery("t".into(), 1)).is_err()
            );
            assert!(matches!(
                refilled.transfer("t", 1),
                Err(StockHistoryError::Invalid)
            ));
            let issued =
                allocate_average(&before, &AverageMovement::Delivery("d".into(), 1))?.state;
            assert!(matches!(
                issued.transfer("d", 1),
                Err(StockHistoryError::Invalid)
            ));
        }
    }
    Ok(())
}
