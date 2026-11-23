#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! `norn-zen/7` temporal interval and DST resolution conformance tests (#1584).

mod support;

use norn_expr::check::{CHECKER_PROFILE, codes};
use norn_expr::{
    EnvOptions, EvaluationBudget, ExprError, Expression, Number, Type, Value, ZenTypeEnv,
    check_expression_with,
};
use norn_ir::TimezonePolicy;
use support::{Model, fields, roomy_limits};

const OUTSIDE: &[&str] = &[codes::UNSUPPORTED_CONSTRUCT, codes::SYNTAX];

fn probe_env() -> ZenTypeEnv {
    let model = Model::retail();
    let defs = norn_expr::EntityDefinitions {
        property_types: &model.property_types,
        interfaces: &model.interfaces,
        object_types: &model.object_types,
        extensions: &model.extensions,
    };
    ZenTypeEnv::from_artifact_set(&defs, EnvOptions::default())
        .expect("env")
        .bind_input(fields([(
            "timestamp_utc",
            Type::Timestamp {
                timezone: TimezonePolicy::Utc,
            },
        )]))
        .expect("input")
}

fn verdict(source: &str, expected: &Type) -> Result<(), Vec<&'static str>> {
    let expr = match Expression::compile(source) {
        Ok(expr) => expr,
        Err(_) => return Err(vec![codes::SYNTAX]),
    };
    check_expression_with(&expr, &probe_env(), expected, &roomy_limits())
        .map(|_| ())
        .map_err(|diagnostics| diagnostics.iter().map(|d| d.code).collect())
}

#[track_caller]
fn refused(source: &str, expected: &Type, allowed: &[&str]) {
    match verdict(source, expected) {
        Ok(()) => panic!("`{source}` is outside {CHECKER_PROFILE} but was admitted"),
        Err(found) => assert!(
            found.iter().any(|code| allowed.contains(code)),
            "`{source}`: expected one of {allowed:?}, got {found:?}"
        ),
    }
}

fn num(text: &str) -> Value {
    Value::Number(text.parse::<Number>().unwrap())
}

fn eval(source: &str, input: Value) -> Result<Value, ExprError> {
    Expression::compile(source)?
        .evaluate(
            &Value::object([("input", input)]),
            EvaluationBudget::default(),
        )
        .map(|evaluation| evaluation.value().clone())
}

#[test]
fn zen7_temporal_intervals_dst_resolution_and_no_epoch_builtins() {
    let env = probe_env();
    let limits = roomy_limits();

    // Positive admission + metered execution within static cost bound.
    for (source, expected_ty, expected_val) in [
        (
            "intervals_overlap(timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T18:00:00Z'), timestamp('2025-03-09T16:00:00Z'), timestamp('2025-03-09T20:00:00Z'))",
            Type::Bool,
            Value::Bool(true),
        ),
        (
            "intervals_overlap(timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T16:00:00Z'), timestamp('2025-03-09T16:00:00Z'), timestamp('2025-03-09T18:00:00Z'))",
            Type::Bool,
            Value::Bool(false),
        ),
        (
            "interval_gap_seconds(timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T15:00:00.25Z'), timestamp('2025-03-09T15:00:01.75Z'), timestamp('2025-03-09T16:00:00Z'))",
            Type::Number,
            num("1.5"),
        ),
        (
            "interval_intersection_seconds(timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T18:00:00Z'), timestamp('2025-03-09T16:30:00Z'), timestamp('2025-03-09T20:00:00Z'))",
            Type::Number,
            num("5400"),
        ),
        (
            "resolve_local_timestamp('2025-11-02T01:30:00', 'America/New_York', 'earlier') < resolve_local_timestamp('2025-11-02T01:30:00', 'America/New_York', 'later')",
            Type::Bool,
            Value::Bool(true),
        ),
    ] {
        let expr = Expression::compile(source).expect("compile");
        let checked = check_expression_with(&expr, &env, &expected_ty, &limits)
            .unwrap_or_else(|diags| panic!("{source}: {diags:?}"));
        let evaluation = expr
            .evaluate(
                &Value::object([("input", Value::object::<&str>([]))]),
                EvaluationBudget::new(checked.cost().units()),
            )
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        assert_eq!(evaluation.value(), &expected_val, "{source}");
    }

    // Runtime refusal on improper intervals and DST edge cases.
    for failing in [
        "intervals_overlap(timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T15:00:00Z'), timestamp('2025-03-09T16:00:00Z'))",
        "interval_gap_seconds(timestamp('2025-03-09T15:00:00Z'), timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T15:00:00Z'), timestamp('2025-03-09T16:00:00Z'))",
        "interval_intersection_seconds(timestamp('2025-03-09T14:00:00Z'), timestamp('2025-03-09T15:00:00Z'), timestamp('2025-03-09T16:00:00Z'), timestamp('2025-03-09T15:00:00Z'))",
        "resolve_local_timestamp('2025-03-09T02:30:00', 'America/New_York', 'reject')",
        "resolve_local_timestamp('2025-03-09T02:30:00', 'America/New_York', 'earlier')",
        "resolve_local_timestamp('2025-03-09T02:30:00', 'America/New_York', 'later')",
        "resolve_local_timestamp('2025-11-02T01:30:00', 'America/New_York', 'reject')",
    ] {
        assert!(
            matches!(
                eval(failing, Value::object::<&str>([])),
                Err(ExprError::Evaluation { .. })
            ),
            "expected runtime evaluation refusal for {failing}"
        );
    }

    // Static type / literal refusals: mixed date/timestamp, raw strings, invalid tz, Factory, bad policy, offset-bearing local time.
    for bad in [
        "intervals_overlap(date('2025-03-09'), timestamp('2025-03-09T15:00:00Z'), timestamp('2025-03-09T15:00:00Z'), timestamp('2025-03-09T16:00:00Z'))",
        "intervals_overlap(input.timestamp_utc, input.timestamp_utc, input.timestamp_utc, input.timestamp_utc)",
        "interval_gap_seconds(date('2025-03-09'), date('2025-03-10'), date('2025-03-11'), date('2025-03-12'))",
        "resolve_local_timestamp('2025-01-15T09:00:00Z', 'UTC', 'reject')",
        "resolve_local_timestamp('2025-01-15T09:00:00', 'Mars/Olympus', 'reject')",
        "resolve_local_timestamp('2025-01-15T09:00:00', 'Factory', 'reject')",
        "resolve_local_timestamp('2025-01-15T09:00:00', 'UTC', 'shift')",
    ] {
        refused(bad, &Type::Json, &[codes::TYPE_ERROR]);
    }

    // Anti-cheat guardrail: no epoch-seconds / epoch-millis conversion builtins exist.
    for forbidden in [
        "epoch_seconds(timestamp('2025-03-09T14:00:00Z'))",
        "epoch_millis(timestamp('2025-03-09T14:00:00Z'))",
        "unix_timestamp(timestamp('2025-03-09T14:00:00Z'))",
        "to_epoch(timestamp('2025-03-09T14:00:00Z'))",
        "from_epoch(1741528800)",
    ] {
        refused(forbidden, &Type::Json, OUTSIDE);
    }
}
