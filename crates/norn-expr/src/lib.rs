//! `norn-expr`: Norn's wrapper over ZEN Expression.
//!
//! Ring 0 — pure, portable semantic core (see `AGENTS.md`). This crate is the
//! only Norn crate that depends on `zen-expression`, pinned to the
//! `Norn-cloud/zen` fork (D19, D32; `zen-fork-ledger.md`). Its public API is
//! Norn-owned: [`Expression`], [`Value`], [`Number`], [`EvaluationBudget`],
//! [`Evaluation`], [`EvaluationAttempt`], [`UnaryTest`], [`ExprError`], the type environment
//! ([`ZenTypeEnv`], [`Type`], [`RecordType`]), and the checker
//! ([`check_expression`], [`check_unary`], [`InferredType`], [`Diagnostic`]). No `zen-*` type crosses this boundary,
//! so an engine change never ripples into the kernel (SKELETON.md; the
//! `public-api-boundary` CI job checks this with `cargo public-api`).
//!
//! - [`Expression`] compiles and evaluates standard expressions under an
//!   explicit operation budget (T1-EXPR-1); [`EvaluationAttempt`] preserves
//!   consumed units when evaluation returns an error.
//! - [`ZenTypeEnv::from_artifact_set`] derives the closed, versioned,
//!   content-addressed type environment of an `EntityIR` v2 artifact set, with
//!   the closed [`actor`] operand (T1-EXPR-2, #2419).
//! - [`check_expression`] type-checks an expression against it under the
//!   `norn-zen/6` profile: the `norn-expr/4` structural grammar with explicit
//!   temporal constructors, same-kind ordering and checked arithmetic,
//!   and bounded string normalization
//!   ([`check`]). [`check_unary`] checks
//!   a decision-table [`UnaryTest`] against its column's subject type the same
//!   way (T1-EXPR-3b, #2521).
//! - [`temporal`] exposes pure validation for `EntityIR` dates and strict
//!   RFC 3339 timestamps plus canonical UTC timestamp formatting. Entity
//!   temporal properties remain JSON strings; checked expression temporals
//!   are opaque ZEN values internally and project to canonical JSON strings.
//!
//! - Every admitted expression is structurally bounded and has a static
//!   [`CostEstimate`] within the limit profile's decide fuel budget, and
//!   evaluation is always metered (T1-EXPR-4, #2522).
//!
//! The versioned Norn profile is published in `PROFILE.md` (T1-EXPR-6).
//! Compilation alone does not imply admission: the checker proves the profile
//! grammar, types, and static cost against the declared environment.
//!
//! ```
//! use norn_expr::{EvaluationBudget, Expression, Number, Value};
//!
//! let expr = Expression::compile("price * quantity")?;
//! let context = Value::object([
//!     ("price", Value::Number("12.50".parse()?)),
//!     ("quantity", Value::Number(Number::from(3))),
//! ]);
//! let evaluation = expr.evaluate(&context, EvaluationBudget::new(100))?;
//! assert_eq!(evaluation.value(), &Value::Number("37.5".parse()?));
//! # Ok::<(), norn_expr::ExprError>(())
//! ```

#![forbid(unsafe_code)]

pub mod actor;
pub mod check;
mod convert;
pub mod env;
mod error;
mod expression;
mod reads;
/// Exact validation of the admitted closed JSON schema dialect.
pub mod schema;
pub mod temporal;
mod value;

pub use check::{
    CheckedExpression, ConstructionSource, CostEstimate, Diagnostic, InferredType, RootCostBounds,
    StructuralLimits, check_expression, check_expression_with, check_expression_with_root_bounds,
    check_expression_with_sources, check_unary, check_unary_with, validate_evaluation_source,
    validate_unary_evaluation_source,
};
pub use env::{
    ACTOR_VARIABLE, EntityDefinitions, EnvError, EnvOptions, Field, FieldSource, INPUT_VARIABLE,
    RecordKind, RecordType, SELF_VARIABLE, TYPE_ENV_HASH_PROFILE, TYPE_ENV_ID, TYPE_ENV_VERSION,
    Type, TypeEnvRef, ZenTypeEnv,
};
pub use error::ExprError;
pub use expression::{
    Evaluation, EvaluationAttempt, EvaluationBudget, Expression, UnaryTest, check_unary_syntax,
};
pub use reads::{ExpressionReads, expression_read_paths, expression_reads};
pub use value::{Number, Value};
