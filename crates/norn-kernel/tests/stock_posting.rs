//! Pure conformance witnesses; storage obligations remain in the action suite.
#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "explicit conformance assertions"
)]
use norn_ir::posting::AccountingAccountRef;
use norn_ir::quantity_ledger::QuantityOperation;
use norn_kernel::stock_fifo::{FifoError, FifoMovement, replay_fifo};
use norn_kernel::stock_posting::{
    StockPostBindings, StockPostError, StockPostLine,
    StockPostOperation::{Delivery, Receipt},
    lower_stock_posting,
};

fn bindings() -> StockPostBindings {
    let account = |id: &str| AccountingAccountRef {
        scope_key: "demo".into(),
        book_id: "book".into(),
        graph_id: "graph".into(),
        graph_version: 1,
        account_id: id.into(),
    };
    StockPostBindings {
        templates: ["receipt".into(), "delivery".into()],
        accounts: [account("stock"), account("srbnb"), account("cogs")],
        currency: "USD".into(),
        layer: "posted".into(),
    }
}
fn line(id: &str, quantity: u128, unit_cost: Option<u128>) -> StockPostLine {
    StockPostLine {
        id: id.into(),
        quantity,
        unit_cost,
    }
}

#[test]
fn post_value_order_identity_cp1_fifo() -> Result<(), StockPostError> {
    let b = bindings();
    let lines = [
        line("a", 2_000_000, Some(10_000_000)),
        line("b", 3_000_000, Some(20_000_000)),
    ];
    let receipt = lower_stock_posting(&b, Receipt, "receive", &[], &lines)?;
    assert_eq!(receipt.gl[0].amount_unscaled, "8000");
    assert_eq!(receipt.gl[1].amount_unscaled, "-8000");
    assert_eq!(receipt.gl[0].account, b.accounts[0]);
    assert_eq!(receipt.gl[1].account, b.accounts[1]);
    assert_eq!(receipt.gl[0].unit_id, "USD");
    assert_eq!(receipt.gl[0].unit_precision, 2);
    assert_eq!(receipt.gl[0].layer_id, "posted");
    assert_eq!(receipt.gl[0].leg_id, "receive/debit");
    assert_eq!(
        Ok(receipt.clone()),
        lower_stock_posting(&b, Receipt, "receive", &[], &lines)
    );
    let mut history: Vec<FifoMovement> =
        receipt.events.iter().map(|e| e.movement.clone()).collect();
    let delivery = lower_stock_posting(
        &b,
        Delivery,
        "deliver",
        &history,
        &[line("c", 4_000_000, None)],
    )?;
    assert_eq!(delivery.gl[0].account, b.accounts[2]);
    assert_eq!(delivery.gl[1].account, b.accounts[0]);
    assert_eq!(delivery.gl[0].amount_unscaled, "6000");
    assert_eq!(delivery.gl[1].amount_unscaled, "-6000");
    assert_eq!(delivery.events[0].allocations[0].value, 20_000_000);
    assert_eq!(delivery.events[0].allocations[1].value, 40_000_000);
    assert_eq!(delivery.layers[0].quantity, 1_000_000);
    assert_eq!(delivery.layers[0].value, 20_000_000);
    history.extend(delivery.events.iter().map(|e| e.movement.clone()));
    assert_eq!(replay_fifo(&history), Ok(delivery.layers.clone()));
    assert!(
        matches!(&delivery.quantity, QuantityOperation::Post { template_id, parameters, .. } if template_id == "delivery" && parameters[0].unscaled == "4000000" && parameters[0].scale == 6)
    );
    Ok(())
}

#[test]
fn post_value_aggregate_half_cents_and_zero_cost() -> Result<(), StockPostError> {
    let b = bindings();
    let receipt = lower_stock_posting(
        &b,
        Receipt,
        "round",
        &[],
        &[
            line("a", 1_000_000, Some(5_000)),
            line("b", 1_000_000, Some(5_000)),
        ],
    )?;
    assert_eq!(
        receipt.gl[0].amount_unscaled, "1",
        "aggregate before rounding"
    );
    let history: Vec<_> = receipt.events.iter().map(|e| e.movement.clone()).collect();
    let delivery = lower_stock_posting(
        &b,
        Delivery,
        "issue",
        &history,
        &[line("c", 1_000_000, None)],
    )?;
    assert_eq!(
        delivery.gl[0].amount_unscaled, "0",
        "round(after) - round(before), not round(issue)"
    );
    let free = lower_stock_posting(&b, Receipt, "free", &[], &[line("a", 1_000_000, Some(0))])?;
    assert_eq!(free.gl[0].amount_unscaled, "0");
    Ok(())
}

#[test]
fn post_atomic_pure_refusal_and_authority_bindings() {
    let mut b = bindings();
    let history = vec![FifoMovement::Receipt(norn_kernel::stock_fifo::FifoLayer {
        receipt: "old".into(),
        quantity: 1_000_000,
        value: 1_000_000,
    })];
    let before = history.clone();
    assert_eq!(
        lower_stock_posting(
            &b,
            Delivery,
            "too-many",
            &history,
            &[line("a", 500_000, None), line("b", 1_000_000, None)]
        ),
        Err(StockPostError::Fifo(FifoError::InsufficientStock))
    );
    assert_eq!(
        history, before,
        "POST-ATOMIC: earlier candidate line cannot mutate history"
    );
    b.accounts[1].graph_version = 2;
    assert_eq!(
        lower_stock_posting(&b, Receipt, "mixed", &[], &[line("a", 1, Some(1))]),
        Err(StockPostError::Invalid)
    );
    for (quantity, cost) in [(0, Some(0)), (1, None), (2, Some(u128::MAX))] {
        assert!(
            lower_stock_posting(
                &bindings(),
                Receipt,
                "invalid",
                &[],
                &[line("a", quantity, cost)]
            )
            .is_err()
        );
    }
    assert!(
        lower_stock_posting(
            &bindings(),
            Delivery,
            "forged-cost",
            &history,
            &[line("a", 1, Some(1))]
        )
        .is_err()
    );
}
