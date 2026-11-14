//! CONS-LINEAR-1 decision core: one writer's weighted coefficient balance.
//!
//! This file imports nothing from the crate, so the pinned Charon/Aeneas
//! extract the production body standalone (`scripts/verify-checker-certificates-aeneas.sh`).
//! The glue in `conservation.rs` supplies exact rationals and the scope/unit
//! partition of every coordinate; this core decides acceptance.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Aeneas models indexing and `+` as checked; the Lean proof shows neither fails"
)]

/// Exact arithmetic the decision needs; `None` is a representation limit.
pub trait ExactArithmetic: Copy {
    /// The additive identity.
    fn exact_zero() -> Self;
    /// Whether the value is exactly zero.
    fn exact_is_zero(self) -> bool;
    /// The exact sum, or `None` at the representation limit.
    fn exact_add(self, rhs: Self) -> Option<Self>;
    /// The exact product, or `None` at the representation limit.
    fn exact_mul(self, rhs: Self) -> Option<Self>;
}

/// Why one writer's delta does not certify the functional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal<N> {
    /// Inconsistent lengths, an empty row width or a partition label out of range.
    Malformed,
    /// A weighted coefficient or partial sum exceeded the representation.
    Overflow,
    /// Some weighted column total is nonzero; the totals are kept for diagnosis.
    Unbalanced(Vec<Vec<N>>),
}

/// CONS-LINEAR-1 for one writer. `partition[i]` labels coordinate `i`'s
/// scope/unit class; `rows[i]` is its affine delta (parameter columns, then the
/// constant). Accepts only when, in every class and every column, the weighted
/// coefficients sum to exactly zero.
///
/// # Errors
/// Malformed shape, representation overflow, or a nonzero weighted total.
pub fn check_writer<N: ExactArithmetic>(
    partition: &[usize],
    weights: &[N],
    rows: &[Vec<N>],
    columns: usize,
) -> Result<(), Refusal<N>> {
    if columns == 0
        || weights.len() != partition.len()
        || rows.len() != partition.len()
        || !labels_in_range(partition)
        || !rows_have_width(rows, columns)
    {
        return Err(Refusal::Malformed);
    }
    let totals = weighted_totals(partition, weights, rows, columns)?;
    if all_zero(&totals) {
        Ok(())
    } else {
        Err(Refusal::Unbalanced(totals))
    }
}

fn labels_in_range(partition: &[usize]) -> bool {
    let mut i = 0;
    while i < partition.len() {
        if partition[i] >= partition.len() {
            return false;
        }
        i += 1;
    }
    true
}

fn rows_have_width<N>(rows: &[Vec<N>], columns: usize) -> bool {
    let mut i = 0;
    while i < rows.len() {
        if rows[i].len() != columns {
            return false;
        }
        i += 1;
    }
    true
}

fn zero_row<N: ExactArithmetic>(columns: usize) -> Vec<N> {
    let mut row = Vec::new();
    let mut c = 0;
    while c < columns {
        row.push(N::exact_zero());
        c += 1;
    }
    row
}

fn zero_totals<N: ExactArithmetic>(classes: usize, columns: usize) -> Vec<Vec<N>> {
    let mut totals = Vec::new();
    let mut r = 0;
    while r < classes {
        totals.push(zero_row(columns));
        r += 1;
    }
    totals
}

/// `total[c] += weight * row[c]` for every column, in column order.
fn add_weighted_row<N: ExactArithmetic>(
    total: &mut [N],
    row: &[N],
    weight: N,
) -> Result<(), Refusal<N>> {
    let mut c = 0;
    while c < row.len() {
        let Some(product) = row[c].exact_mul(weight) else {
            return Err(Refusal::Overflow);
        };
        let Some(sum) = total[c].exact_add(product) else {
            return Err(Refusal::Overflow);
        };
        total[c] = sum;
        c += 1;
    }
    Ok(())
}

/// Per class label, the weighted column totals, accumulated in coordinate order.
fn weighted_totals<N: ExactArithmetic>(
    partition: &[usize],
    weights: &[N],
    rows: &[Vec<N>],
    columns: usize,
) -> Result<Vec<Vec<N>>, Refusal<N>> {
    let mut totals = zero_totals(partition.len(), columns);
    let mut i = 0;
    while i < partition.len() {
        add_weighted_row(&mut totals[partition[i]], &rows[i], weights[i])?;
        i += 1;
    }
    Ok(totals)
}

fn row_is_zero<N: ExactArithmetic>(row: &[N]) -> bool {
    let mut c = 0;
    while c < row.len() {
        if !row[c].exact_is_zero() {
            return false;
        }
        c += 1;
    }
    true
}

fn all_zero<N: ExactArithmetic>(totals: &[Vec<N>]) -> bool {
    let mut r = 0;
    while r < totals.len() {
        if !row_is_zero(&totals[r]) {
            return false;
        }
        r += 1;
    }
    true
}
