//! COMPUTE-DETERMINISTIC-1 decision core: per-node DBSP algebra verdicts.
//!
//! This file imports nothing from the crate, so the pinned Charon/Aeneas
//! extract the production body standalone (`scripts/verify-checker-certificates-aeneas.sh`).
//! `certificate.rs` re-exports the operator types and maps each verdict to its
//! public `Algebraic` value.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "Aeneas models indexing and `+` as checked; the Lean proof shows neither fails"
)]

/// Closed homomorphisms; these are mathematical operations, not user assertions.
#[derive(Debug, Clone, Copy)]
pub enum Homomorphism {
    /// Fixed predicate on rows, extended by signed multiplicity.
    Select,
    /// Fixed row mapping, summing multiplicities on collisions.
    Project,
    /// Exact keyed sum, without rounding.
    GroupSum,
    /// Natural multiplicity sum; valid only for insert-only changes.
    MonoidSum,
}

/// One dependency-ordered operator. References must point strictly backwards.
#[derive(Debug, Clone)]
pub enum Operator {
    /// Signed multiset input (Z-set).
    Input,
    /// A declared closed homomorphism, including folds with these exact laws.
    Homomorphism {
        /// Prior operator index.
        input: usize,
        /// Absent declarations never inherit a law from sampled behavior.
        law: Option<Homomorphism>,
    },
    /// Bilinear equijoin, with coefficients for J(dL,R), J(L,dR), J(dL,dR).
    Join {
        /// Prior left operator.
        left: usize,
        /// Prior right operator.
        right: usize,
        /// Old snapshots on both sides; the cross term is mandatory.
        coefficients: [i8; 3],
    },
    /// Semantics outside the supported fragment.
    Unknown,
}

/// One node's verdict; `certificate.rs` attaches the public reason text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The node matches a closed algebraic rule.
    Proved,
    /// A bilinear expansion has an incorrect coefficient at this term.
    Refuted(usize),
    /// More than 256 operators.
    OverBudget,
    /// A homomorphism without a declared law.
    UndeclaredFold,
    /// A monoid sum under signed changes.
    MonoidUnderDeletions,
    /// An operator outside the supported fragment.
    Unsupported,
    /// A dependency that is not strictly earlier or not itself proved.
    InvalidDependency,
}

/// The largest composition checked; larger ones are all `OverBudget`.
pub const OPERATOR_BUDGET: usize = 256;

fn proved_before(results: &[Verdict], dependency: usize) -> bool {
    dependency < results.len() && results[dependency] == Verdict::Proved
}

fn homomorphism(law: Option<Homomorphism>, signed_changes: bool) -> Verdict {
    match law {
        None => Verdict::UndeclaredFold,
        Some(Homomorphism::MonoidSum) if signed_changes => Verdict::MonoidUnderDeletions,
        Some(
            Homomorphism::Select
            | Homomorphism::Project
            | Homomorphism::GroupSum
            | Homomorphism::MonoidSum,
        ) => Verdict::Proved,
    }
}

/// The first coefficient that is not 1, as a refuted term.
fn join(coefficients: [i8; 3]) -> Verdict {
    if coefficients[0] != 1 {
        Verdict::Refuted(0)
    } else if coefficients[1] != 1 {
        Verdict::Refuted(1)
    } else if coefficients[2] != 1 {
        Verdict::Refuted(2)
    } else {
        Verdict::Proved
    }
}

fn node(results: &[Verdict], operator: &Operator, signed_changes: bool) -> Verdict {
    match operator {
        Operator::Input => Verdict::Proved,
        Operator::Homomorphism { input, law } => {
            if proved_before(results, *input) {
                homomorphism(*law, signed_changes)
            } else {
                Verdict::InvalidDependency
            }
        }
        Operator::Join {
            left,
            right,
            coefficients,
        } => {
            if proved_before(results, *left) && proved_before(results, *right) {
                join(*coefficients)
            } else {
                Verdict::InvalidDependency
            }
        }
        Operator::Unknown => Verdict::Unsupported,
    }
}

fn over_budget(count: usize) -> Vec<Verdict> {
    let mut results = Vec::new();
    let mut index = 0;
    while index < count {
        results.push(Verdict::OverBudget);
        index += 1;
    }
    results
}

/// One verdict per operator, in order. A node is proved only if every operator
/// it reads is strictly earlier and already proved.
#[must_use]
pub fn check(operators: &[Operator], signed_changes: bool) -> Vec<Verdict> {
    if operators.len() > OPERATOR_BUDGET {
        return over_budget(operators.len());
    }
    let mut results = Vec::new();
    let mut index = 0;
    while index < operators.len() {
        let verdict = node(&results, &operators[index], signed_changes);
        results.push(verdict);
        index += 1;
    }
    results
}
