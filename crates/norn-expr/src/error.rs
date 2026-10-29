//! Norn-owned expression errors. zen errors are mapped here and never exposed.
//!
//! `Display`/`Error` are implemented by hand: SKELETON.md admits only
//! `zen-expression` and `norn-ir` as `norn-expr` dependencies, so `thiserror`
//! is not available in this crate.

use std::fmt;

/// An error from compiling or evaluating a ZEN expression.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExprError {
    /// The source failed to lex, parse, or compile.
    Compile {
        /// Engine diagnostic (not a stable contract).
        message: String,
    },
    /// Evaluation failed (type error, bad conversion, invalid date, ...).
    Evaluation {
        /// Engine diagnostic (not a stable contract).
        message: String,
    },
    /// Evaluation needed more operation units than its [`EvaluationBudget`].
    ///
    /// Unit counts are deterministic: they depend only on the expression and
    /// its input, identically natively and on `wasm32-unknown-unknown`.
    ///
    /// [`EvaluationBudget`]: crate::EvaluationBudget
    BudgetExhausted {
        /// The budget's unit limit.
        limit: u64,
        /// Units the failing charge would have reached (`used > limit`).
        used: u64,
    },
    /// A decimal string is not an exact, supported Norn number.
    InvalidNumber {
        /// The rejected input.
        input: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The expression produced a value with no Norn [`Value`] representation.
    ///
    /// [`Value`]: crate::Value
    UnsupportedResult {
        /// Engine type name of the value (for example `"date"`).
        type_name: String,
    },
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile { message } => write!(f, "expression failed to compile: {message}"),
            Self::Evaluation { message } => write!(f, "expression evaluation failed: {message}"),
            Self::BudgetExhausted { limit, used } => write!(
                f,
                "expression evaluation budget exhausted: {used} units needed, limit {limit}"
            ),
            Self::InvalidNumber { input, reason } => {
                write!(f, "invalid number `{input}`: {reason}")
            }
            Self::UnsupportedResult { type_name } => write!(
                f,
                "expression produced a `{type_name}` value, which has no Norn value representation"
            ),
        }
    }
}

impl std::error::Error for ExprError {}
