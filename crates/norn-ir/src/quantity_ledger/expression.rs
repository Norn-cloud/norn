//! Closed authoring expressions for exact affine ledger amounts.
//!
//! This is a classification and lowering boundary, not a general expression
//! interpreter. Deserialization rejects unknown operations and fields. The
//! lowering pass (`norn_checker::quantity_ledger::lower_ledger_amount_expression`)
//! accepts only literals, declared parameters, addition,
//! subtraction, and multiplication where at most one operand depends on a
//! parameter. Every arithmetic operation and decimal rescale is checked.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Wire version of the closed ledger amount authoring language.
pub const LEDGER_AMOUNT_EXPRESSION_VERSION: &str = "1.0";

/// Versioned source expression and its exact requested result scale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerAmountExpressionIr {
    pub expression_version: String,
    pub result_scale: u8,
    pub expression: LedgerAmountExpressionNode,
}

/// The complete v1 authoring grammar.
///
/// `multiply` is decoded so the classifier can return a specific nonlinear
/// error. It is admitted only when at most one operand transitively references
/// a parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LedgerAmountExpressionNode {
    Literal { unscaled: String, scale: u8 },
    Parameter { parameter_id: String },
    Add { left: Box<Self>, right: Box<Self> },
    Subtract { left: Box<Self>, right: Box<Self> },
    Multiply { left: Box<Self>, right: Box<Self> },
}

/// Stable machine-readable lowering failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LedgerAmountLoweringErrorCode {
    UnsupportedExpressionVersion,
    EmptyParameterId,
    DuplicateParameter,
    UnknownParameter,
    InvalidInteger,
    NonCanonicalInteger,
    ScaleOutOfRange,
    ScaleLoss,
    NonlinearMultiplication,
    ArithmeticOverflow,
}

/// A deterministic typed failure from expression classification or lowering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerAmountLoweringError {
    pub code: LedgerAmountLoweringErrorCode,
    pub path: String,
    pub message: String,
}
