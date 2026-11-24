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

#[test]
fn versioned_tzdb_2025b_and_2026c_edmonton_regression_and_isolation() {
    use norn_expr::temporal::{
        DstPolicy, NamingSeriesDate, TzdbBundle, naming_series_date_at_in,
        resolve_local_timestamp_in,
    };
    use norn_expr::{UnaryTest, check_expression_with_in};

    let b25 = TzdbBundle::v2025b();
    let b26 = TzdbBundle::v2026c();
    let env = probe_env();
    let limits = roomy_limits();

    // 2027-01-01T06:30:00Z in America/Edmonton:
    // - 2025b: MST (UTC-7) -> 2026-12-31 23:30 (naming-series year 2026)
    // - 2026c: permanent MDT/CST (UTC-6) -> 2027-01-01 00:30 (naming-series year 2027)
    let epoch_ms = 1_798_785_000_000_i64;
    assert_eq!(
        naming_series_date_at_in(epoch_ms, "America/Edmonton", b25),
        Ok(NamingSeriesDate {
            year: 2026,
            month: 12,
            day: 31,
        })
    );
    assert_eq!(
        naming_series_date_at_in(epoch_ms, "America/Edmonton", b26),
        Ok(NamingSeriesDate {
            year: 2027,
            month: 1,
            day: 1,
        })
    );
    assert_eq!(
        resolve_local_timestamp_in(
            "2026-12-31T23:30:00",
            "America/Edmonton",
            DstPolicy::Reject,
            b25
        ),
        Ok("2027-01-01T06:30:00Z".to_owned())
    );
    assert_eq!(
        resolve_local_timestamp_in(
            "2027-01-01T00:30:00",
            "America/Edmonton",
            DstPolicy::Reject,
            b26
        ),
        Ok("2027-01-01T06:30:00Z".to_owned())
    );

    // Same expression checked and evaluated under explicit 2025b vs 2026c bundles.
    let source = "resolve_local_timestamp('2026-12-31T23:30:00', 'America/Edmonton', 'reject')";
    let expr = Expression::compile(source).expect("compile");
    let checked_25 = check_expression_with_in(
        &expr,
        &env,
        &Type::Timestamp {
            timezone: TimezonePolicy::Utc,
        },
        &limits,
        b25,
    )
    .expect("check 2025b");
    let checked_26 = check_expression_with_in(
        &expr,
        &env,
        &Type::Timestamp {
            timezone: TimezonePolicy::Utc,
        },
        &limits,
        b26,
    )
    .expect("check 2026c");
    let root = Value::object([("input", Value::object(std::iter::empty::<(&str, Value)>()))]);

    // Interleaved same-thread evaluations prove zero thread-local/global leakage.
    for _ in 0..4 {
        let out_25 = expr
            .evaluate_in(&root, EvaluationBudget::new(checked_25.cost().units()), b25)
            .expect("eval 2025b");
        let out_26 = expr
            .evaluate_in(&root, EvaluationBudget::new(checked_26.cost().units()), b26)
            .expect("eval 2026c");
        assert_eq!(out_25.value(), &Value::from("2027-01-01T06:30:00Z"));
        assert_eq!(out_26.value(), &Value::from("2027-01-01T05:30:00Z"));
    }

    // UnaryTest::evaluate_in honors explicit bundle selection too.
    let unary = UnaryTest::compile(
        "timestamp($) == resolve_local_timestamp('2026-12-31T23:30:00', 'America/Edmonton', 'reject')",
    )
    .expect("unary compile");
    let subject = Value::from("2027-01-01T06:30:00Z");
    assert!(
        unary
            .evaluate_in(&root, &subject, EvaluationBudget::default(), b25)
            .expect("unary 2025b")
    );
    assert!(
        !unary
            .evaluate_in(&root, &subject, EvaluationBudget::default(), b26)
            .expect("unary 2026c")
    );

    // Concurrent multi-thread evaluations with 2025b and 2026c produce exact independent results.
    let handles: Vec<_> = (0..8)
        .map(|worker| {
            let expr = expr.clone();
            let root = root.clone();
            std::thread::spawn(move || {
                let use_26 = worker % 2 == 1;
                let bundle = if use_26 {
                    TzdbBundle::v2026c()
                } else {
                    TzdbBundle::v2025b()
                };
                let expected = if use_26 {
                    Value::from("2027-01-01T05:30:00Z")
                } else {
                    Value::from("2027-01-01T06:30:00Z")
                };
                for _ in 0..25 {
                    let result = expr
                        .evaluate_in(&root, EvaluationBudget::default(), bundle)
                        .expect("thread eval");
                    assert_eq!(result.value(), &expected);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("worker joined");
    }

    // Literal admission honors the explicit bundle (custom subset bundle with only UTC rejects America/Edmonton).
    let utc_tzif = b25.tzif("UTC").expect("UTC tzif");
    let mut only_utc = Vec::new();
    only_utc.extend_from_slice(norn_expr::temporal::TZDB_BUNDLE_MAGIC);
    only_utc.extend_from_slice(&5_u16.to_le_bytes()); // "2099u"
    only_utc.extend_from_slice(&0_u16.to_le_bytes());
    only_utc.extend_from_slice(&1_u32.to_le_bytes());
    only_utc.extend_from_slice(&3_u32.to_le_bytes());
    only_utc.extend_from_slice(&u32::try_from(utc_tzif.len()).unwrap().to_le_bytes());
    only_utc.extend_from_slice(b"2099u");
    only_utc.extend_from_slice(&0_u32.to_le_bytes());
    only_utc.extend_from_slice(&3_u32.to_le_bytes());
    only_utc.extend_from_slice(&0_u32.to_le_bytes());
    only_utc.extend_from_slice(&u32::try_from(utc_tzif.len()).unwrap().to_le_bytes());
    only_utc.extend_from_slice(b"UTC");
    only_utc.extend_from_slice(utc_tzif);
    let utc_bundle = TzdbBundle::from_bytes(&only_utc).expect("synthetic utc-only bundle");

    assert!(
        check_expression_with_in(
            &expr,
            &env,
            &Type::Timestamp {
                timezone: TimezonePolicy::Utc,
            },
            &limits,
            &utc_bundle,
        )
        .is_err(),
        "America/Edmonton must be rejected by literal admission under a UTC-only bundle"
    );
    let utc_expr =
        Expression::compile("resolve_local_timestamp('2026-12-31T23:30:00', 'UTC', 'reject')")
            .expect("utc expr");
    assert!(
        check_expression_with_in(
            &utc_expr,
            &env,
            &Type::Timestamp {
                timezone: TimezonePolicy::Utc,
            },
            &limits,
            &utc_bundle,
        )
        .is_ok()
    );
}
