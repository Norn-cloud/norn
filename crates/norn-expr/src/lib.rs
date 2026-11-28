//! `norn-expr`: Norn's wrapper over ZEN Expression.
//!
//! Ring 0 — pure, portable semantic core (see `AGENTS.md`). This crate is the
//! only Norn crate that depends on `zen-expression`, pinned to the
//! `Norn-cloud/zen` fork, which carries only engine-internal patches
//! (`crates/norn-skeleton-guard/zen-pin.json`). Its public API is
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
//!   `norn-zen/7` profile: the `norn-expr/4` structural grammar with explicit
//!   temporal constructors, same-kind ordering, checked temporal arithmetic,
//!   proper-interval and pinned-IANA DST builtins, and bounded string normalization
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
//! The versioned Norn profile is published in this crate's `README.md` (T1-EXPR-6).
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

#[cfg(test)]
#[allow(
    dead_code,
    reason = "Compile build-script unit tests in the library test harness"
)]
#[path = "../build.rs"]
mod build_script;

pub mod actor;
pub mod check;
mod convert;
pub mod env;
mod env_index;
mod error;
mod expression;
mod fuel;
pub mod graph;
mod literals;
mod reads;
mod row_tree;
/// Exact validation of the admitted closed JSON schema dialect.
pub mod schema;
pub mod temporal;
mod tzdb;
mod value;

pub use check::{
    CheckedExpression, ConstructionSource, CostEstimate, Diagnostic, InferredType, RootCostBounds,
    StructuralLimits, check_expression, check_expression_in, check_expression_under,
    check_expression_under_in, check_expression_with, check_expression_with_in,
    check_expression_with_root_bounds, check_expression_with_root_bounds_in,
    check_expression_with_sources, check_expression_with_sources_in, check_inherited_expression,
    check_inherited_expression_in, check_inherited_expression_under,
    check_inherited_expression_under_in, check_unary, check_unary_in, check_unary_with,
    check_unary_with_in, condition_null_guards, validate_evaluation_source,
    validate_profile_constructs, validate_unary_evaluation_source,
};
pub use convert::rescale_decimal;
/// Closed server commit clock operand (`ACTION-CLOCK-1`).
pub const CLOCK_VARIABLE: &str = "clock";
/// Record name of the server commit clock bound during action derivation (`ACTION-CLOCK-1`).
pub const ACTION_CLOCK_RECORD: &str = "norn:ActionClock";

pub(crate) fn action_clock_record_fields() -> std::collections::BTreeMap<String, Type> {
    std::collections::BTreeMap::from([
        ("nowMillis".to_owned(), Type::Integer),
        ("nowIso".to_owned(), Type::text()),
    ])
}

pub use env::{
    ACTOR_VARIABLE, EntityDefinitions, EnvError, EnvOptions, Field, FieldSource, INPUT_VARIABLE,
    RecordKind, RecordType, SELF_VARIABLE, TYPE_ENV_HASH_PROFILE, TYPE_ENV_ID, TYPE_ENV_VERSION,
    Type, TypeEnvRef, ZenTypeEnv,
};
pub use error::ExprError;
pub use expression::{
    Evaluation, EvaluationAttempt, EvaluationBudget, Expression, PreparedAdmission, UnaryTest,
    check_unary_syntax,
};
pub use fuel::{TurnFuel, sort_cost};
pub use literals::{FieldValue, ObjectLiteral, command_records, object_literals};
pub use reads::{
    ExpressionReads, expression_element_paths, expression_read_paths,
    expression_read_paths_with_closures, expression_reads, unary_test_reads,
};
pub use row_tree::{RowArithmetic, RowComparison, RowExpr, row_expression};
pub use value::{Number, Value};
