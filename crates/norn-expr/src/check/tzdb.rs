//! Expression and unary-test checking entry points (default `2025b` and explicit [`TzdbBundle`], `#1440`).

use std::collections::BTreeMap;

use norn_ir::ResourceLimitProfile;

use super::{
    CheckOptions, CheckedExpression, ConstructionSource, CostEstimate, Diagnostic, InferredType,
    RootCostBounds, check_expression_with_sources_and_root_bounds, check_guarded,
    check_unary_with_bundle, condition_null_guards,
};
use crate::env::{Type, ZenTypeEnv};
use crate::temporal::TzdbBundle;
use crate::{Expression, UnaryTest};

/// Type-checks `expr` in `env` under [`super::CHECKER_PROFILE`] and checks its
/// result against `expected` under `norn.limits/1` and the default tzdb (`2025b`).
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
) -> Result<InferredType, Vec<Diagnostic>> {
    check_expression_in(expr, env, expected, TzdbBundle::default_bundle())
}

/// [`check_expression`] against an explicit versioned [`TzdbBundle`].
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression_in(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    tzdb: &TzdbBundle,
) -> Result<InferredType, Vec<Diagnostic>> {
    check_expression_with_in(
        expr,
        env,
        expected,
        &ResourceLimitProfile::norn_limits_v1(),
        tzdb,
    )
    .map(CheckedExpression::into_inferred)
}

/// Type-checks `expr` in `env` under [`super::CHECKER_PROFILE`] and checks its
/// result against `expected`, bounding it by `limits`.
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression_with(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
) -> Result<CheckedExpression, Vec<Diagnostic>> {
    check_expression_with_in(expr, env, expected, limits, TzdbBundle::default_bundle())
}

/// [`check_expression_with`] against an explicit versioned [`TzdbBundle`].
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression_with_in(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
    tzdb: &TzdbBundle,
) -> Result<CheckedExpression, Vec<Diagnostic>> {
    check_expression_with_sources_in(expr, env, expected, limits, tzdb).map(|(checked, _)| checked)
}

/// Checks an expression whose named context roots have independent enforced
/// bounds, using the default tzdb (`2025b`).
///
/// # Errors
///
/// Every [`Diagnostic`] found by the expression checker.
pub fn check_expression_with_root_bounds(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
    root_bounds: &BTreeMap<String, RootCostBounds>,
) -> Result<CheckedExpression, Vec<Diagnostic>> {
    check_expression_with_root_bounds_in(
        expr,
        env,
        expected,
        limits,
        root_bounds,
        TzdbBundle::default_bundle(),
    )
}

/// [`check_expression_with_root_bounds`] against an explicit versioned [`TzdbBundle`].
///
/// # Errors
///
/// Every [`Diagnostic`] found by the expression checker.
pub fn check_expression_with_root_bounds_in(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
    root_bounds: &BTreeMap<String, RootCostBounds>,
    tzdb: &TzdbBundle,
) -> Result<CheckedExpression, Vec<Diagnostic>> {
    check_expression_with_sources_and_root_bounds(
        expr,
        env,
        expected,
        limits,
        Some(root_bounds),
        true,
        tzdb,
    )
    .map(|(checked, _)| checked)
}

/// Checks an expression with the same bounds and grammar as
/// [`check_expression_with`], retaining its owned construction sources.
///
/// # Errors
///
/// Every [`Diagnostic`] found by the expression checker.
pub fn check_expression_with_sources(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
) -> Result<(CheckedExpression, Option<ConstructionSource>), Vec<Diagnostic>> {
    check_expression_with_sources_in(expr, env, expected, limits, TzdbBundle::default_bundle())
}

/// [`check_expression_with_sources`] against an explicit versioned [`TzdbBundle`].
///
/// # Errors
///
/// Every [`Diagnostic`] found by the expression checker.
pub fn check_expression_with_sources_in(
    expr: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    limits: &ResourceLimitProfile,
    tzdb: &TzdbBundle,
) -> Result<(CheckedExpression, Option<ConstructionSource>), Vec<Diagnostic>> {
    check_expression_with_sources_and_root_bounds(expr, env, expected, limits, None, false, tzdb)
}

/// [`check_expression`] for an expression evaluated only when a guarding
/// condition holds (a law's response under its condition): every literal read
/// path the condition proves non-null is read as non-null.
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression_under(
    expr: &Expression,
    condition: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
) -> Result<InferredType, Vec<Diagnostic>> {
    check_expression_under_in(expr, condition, env, expected, TzdbBundle::default_bundle())
}

/// [`check_expression_under`] against an explicit versioned [`TzdbBundle`].
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_expression_under_in(
    expr: &Expression,
    condition: &Expression,
    env: &ZenTypeEnv,
    expected: &Type,
    tzdb: &TzdbBundle,
) -> Result<InferredType, Vec<Diagnostic>> {
    let limits = ResourceLimitProfile::norn_limits_v1();
    let assumed = condition_null_guards(condition);
    check_guarded(
        expr,
        env,
        expected,
        &CheckOptions {
            limits: &limits,
            root_bounds: None,
            exact_enabled: false,
            assumed: &assumed,
            tzdb,
        },
    )
    .map(|(checked, _)| checked.into_inferred())
}

/// Type-checks the unary test `test` against a subject `$` of type `subject`
/// in `env`, under the same allowlist as [`check_expression`] and the shipped
/// limit profile `norn.limits/1`.
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_unary(
    test: &UnaryTest,
    env: &ZenTypeEnv,
    subject: &Type,
) -> Result<(), Vec<Diagnostic>> {
    check_unary_in(test, env, subject, TzdbBundle::default_bundle())
}

/// [`check_unary`] against an explicit versioned [`TzdbBundle`].
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_unary_in(
    test: &UnaryTest,
    env: &ZenTypeEnv,
    subject: &Type,
    tzdb: &TzdbBundle,
) -> Result<(), Vec<Diagnostic>> {
    check_unary_with_in(
        test,
        env,
        subject,
        &ResourceLimitProfile::norn_limits_v1(),
        tzdb,
    )
    .map(|_| ())
}

/// Type-checks the unary test `test` against a subject `$` of type `subject`
/// in `env`, under the same allowlist, structural bounds, and cost bound as
/// [`check_expression_with`], and returns its [`CostEstimate`].
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_unary_with(
    test: &UnaryTest,
    env: &ZenTypeEnv,
    subject: &Type,
    limits: &ResourceLimitProfile,
) -> Result<CostEstimate, Vec<Diagnostic>> {
    check_unary_with_in(test, env, subject, limits, TzdbBundle::default_bundle())
}

/// [`check_unary_with`] against an explicit versioned [`TzdbBundle`].
///
/// # Errors
///
/// Every [`Diagnostic`] found.
pub fn check_unary_with_in(
    test: &UnaryTest,
    env: &ZenTypeEnv,
    subject: &Type,
    limits: &ResourceLimitProfile,
    tzdb: &TzdbBundle,
) -> Result<CostEstimate, Vec<Diagnostic>> {
    check_unary_with_bundle(test, env, subject, limits, tzdb)
}
