//! Pure receipt/delivery preparation; verification/posting-v1/lowering.md.
use std::collections::BTreeSet;

use norn_ir::posting::{AccountingAccountRef, AccountingLegIr};
use norn_ir::quantity_ledger::{LedgerParameterValue, QuantityOperation};

use crate::stock_fifo::{FifoError, FifoLayer, FifoMovement, allocate_fifo, replay_fifo};

/// Closed operation selected by the admitted posting rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockPostOperation {
    /// External source to stock, with acquisition cost from owned lines.
    Receipt,
    /// Stock to external destination, with cost from FIFO alone.
    Delivery,
}

/// Master-selected account and template bindings for one stock partition.
#[derive(Debug, Clone)]
pub struct StockPostBindings {
    /// Quantity templates for receipt and delivery, respectively.
    pub templates: [String; 2],
    /// Stock, SRBNB and COGS account references, respectively.
    pub accounts: [AccountingAccountRef; 3],
    /// Currency at precision two.
    pub currency: String,
    /// Accounting layer shared by both legs.
    pub layer: String,
}

/// One authorized owned line in manifest order; integers are millionths.
#[derive(Debug, Clone)]
pub struct StockPostLine {
    /// Stable owned line identity within this posting.
    pub id: String,
    /// Positive quantity.
    pub quantity: u128,
    /// Acquisition unit cost for receipts; absent for deliveries.
    pub unit_cost: Option<u128>,
}

/// Immutable movement and its source allocations, produced together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockPostEvent {
    /// Stable posting/line identity, independent of allocation order.
    pub id: String,
    /// Exact movement to replay in commit order.
    pub movement: FifoMovement,
    /// FIFO source allocations; empty for a receipt.
    pub allocations: Vec<FifoLayer>,
}

/// One complete proposal; this value grants no commit authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockPostPlan {
    /// Remaining FIFO layers.
    pub layers: Vec<FifoLayer>,
    /// Movements in authoritative manifest order.
    pub events: Vec<StockPostEvent>,
    /// Conserved quantity template, parameterized only by derived quantity.
    pub quantity: QuantityOperation,
    /// Debit and credit legs; positive amounts are debits.
    pub gl: [AccountingLegIr; 2],
}

/// Ordinary preparation refusal; no input is mutated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StockPostError {
    /// Invalid identities, quantities, account pins, bounds or line shape.
    #[error("invalid stock posting")]
    Invalid,
    /// Exact arithmetic exceeds the representable range.
    #[error("stock posting arithmetic overflow")]
    Overflow,
    /// History or a movement failed the shared FIFO contract.
    #[error(transparent)]
    Fifo(#[from] FifoError),
}

fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn total_value(layers: &[FifoLayer]) -> Result<u128, StockPostError> {
    layers.iter().try_fold(0_u128, |sum, layer| {
        sum.checked_add(layer.value).ok_or(StockPostError::Overflow)
    })
}

fn round(value: u128, divisor: u128) -> Result<u128, StockPostError> {
    let half = divisor.checked_div(2).ok_or(StockPostError::Overflow)?;
    value
        .checked_div(divisor)
        .and_then(|whole| whole.checked_add(u128::from(value.checked_rem(divisor)? >= half)))
        .ok_or(StockPostError::Overflow)
}

fn cents(value: u128) -> Result<i128, StockPostError> {
    i128::try_from(round(value, 10_000)?).map_err(|_range| StockPostError::Overflow)
}

fn validate_bindings(bindings: &StockPostBindings) -> Result<(), StockPostError> {
    let [stock, srbnb, cogs] = &bindings.accounts;
    let [receipt_template, delivery_template] = &bindings.templates;
    if !identity(&bindings.currency)
        || !identity(&bindings.layer)
        || bindings.templates.iter().any(|id| !identity(id))
        || receipt_template == delivery_template
        || bindings.accounts.iter().any(|a| {
            !identity(&a.account_id)
                || !identity(&a.scope_key)
                || !identity(&a.book_id)
                || !identity(&a.graph_id)
                || a.scope_key != stock.scope_key
                || a.book_id != stock.book_id
                || a.graph_id != stock.graph_id
                || a.graph_version != stock.graph_version
        })
        || stock.account_id == srbnb.account_id
        || stock.account_id == cogs.account_id
        || srbnb.account_id == cogs.account_id
    {
        return Err(StockPostError::Invalid);
    }
    Ok(())
}

/// Derive quantity, immutable movements and GL together using the shared FIFO API.
/// History and lines must already be authorized and fenced by the action caller.
///
/// # Errors
/// Invalid bindings/lines, arithmetic overflow or FIFO refusal return no proposal.
pub fn lower_stock_posting(
    bindings: &StockPostBindings,
    operation: StockPostOperation,
    posting_id: &str,
    history: &[FifoMovement],
    lines: &[StockPostLine],
) -> Result<StockPostPlan, StockPostError> {
    if !identity(posting_id) || lines.is_empty() || lines.len() > 100 || history.len() > 10_000 {
        return Err(StockPostError::Invalid);
    }
    validate_bindings(bindings)?;
    let [stock, srbnb, cogs] = &bindings.accounts;
    let [receipt_template, delivery_template] = &bindings.templates;
    let mut layers = replay_fifo(history)?;
    let before = cents(total_value(&layers)?)?;
    let mut quantity = 0_u128;
    let mut seen = BTreeSet::new();
    let mut events = Vec::with_capacity(lines.len());
    for line in lines {
        if !identity(&line.id) || !seen.insert(&line.id) || line.quantity == 0 {
            return Err(StockPostError::Invalid);
        }
        quantity = quantity
            .checked_add(line.quantity)
            .ok_or(StockPostError::Overflow)?;
        let id = norn_ir::canonical::canonical_json_of(&(posting_id, &line.id))
            .map_err(|_encoding| StockPostError::Invalid)?;
        let id = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&id);
        let movement = match (operation, line.unit_cost) {
            (StockPostOperation::Receipt, Some(cost)) => {
                let product = line
                    .quantity
                    .checked_mul(cost)
                    .ok_or(StockPostError::Overflow)?;
                let value = round(product, 1_000_000)?;
                FifoMovement::Receipt(FifoLayer {
                    receipt: id.clone(),
                    quantity: line.quantity,
                    value,
                })
            }
            (StockPostOperation::Delivery, None) => FifoMovement::Delivery(line.quantity),
            _ => return Err(StockPostError::Invalid),
        };
        let result = allocate_fifo(&layers, &movement)?;
        layers = result.layers;
        events.push(StockPostEvent {
            id,
            movement,
            allocations: result.allocations,
        });
    }
    let quantity = i128::try_from(quantity).map_err(|_range| StockPostError::Overflow)?;
    let stock_delta = cents(total_value(&layers)?)?
        .checked_sub(before)
        .ok_or(StockPostError::Overflow)?;
    let (template, debit, credit, amount) = match operation {
        StockPostOperation::Receipt => (receipt_template, stock, srbnb, stock_delta),
        StockPostOperation::Delivery => (
            delivery_template,
            cogs,
            stock,
            stock_delta.checked_neg().ok_or(StockPostError::Overflow)?,
        ),
    };
    let leg = |suffix: &str, account: &AccountingAccountRef, amount: i128| AccountingLegIr {
        leg_id: format!("{posting_id}/{suffix}"),
        account: account.clone(),
        unit_id: bindings.currency.clone(),
        unit_precision: 2,
        layer_id: bindings.layer.clone(),
        amount_unscaled: amount.to_string(),
    };
    Ok(StockPostPlan {
        layers,
        events,
        quantity: QuantityOperation::Post {
            posting_id: posting_id.into(),
            template_id: template.clone(),
            parameters: vec![LedgerParameterValue {
                parameter_id: "amount".into(),
                scale: 6,
                unscaled: quantity.to_string(),
            }],
        },
        gl: [
            leg("debit", debit, amount),
            leg(
                "credit",
                credit,
                amount.checked_neg().ok_or(StockPostError::Overflow)?,
            ),
        ],
    })
}
