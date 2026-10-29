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

//! Conformance with the published profile `norn-zen/6` (`PROFILE.md`,
//! T1-EXPR-6 #2766): positive and negative fixtures per rule section, the
//! zen adoption review's mandatory counterexamples, and the pin that ties
//! `PROFILE.md`'s content to the profile id.

mod support;

use norn_expr::check::{CHECKER_PROFILE, StructuralLimits, codes};
use norn_expr::{
    EnvOptions, EvaluationBudget, ExprError, Expression, Number, Type, UnaryTest, Value,
    ZenTypeEnv, check_expression_with, check_unary_with, validate_evaluation_source,
    validate_unary_evaluation_source,
};
use norn_ir::{ObjectTypeDef, ResourceLimitProfile, TimezonePolicy};
use support::{Model, fields, roomy_limits};

/// The Norn1 hash of `PROFILE.md`, pinned with the profile id: editing the
/// profile without bumping `CHECKER_PROFILE` (and re-pinning) fails here.
const PROFILE_MD_HASH: &str = "bd12ad3c7cd74e5881b62d3e781b535ac8aa10cf68934ef28938b0c76c481813";

fn probe_env() -> ZenTypeEnv {
    let model = Model::retail();
    let mut object_types: Vec<ObjectTypeDef> = model.object_types.clone();
    let mut properties: Vec<serde_json::Value> = (0..40)
        .map(
            |index| serde_json::json!({ "id": format!("p{index:02}"), "type": { "kind": "text" } }),
        )
        .collect();
    properties.push(serde_json::json!({ "id": "x", "type": { "kind": "text" } }));
    object_types.push(
        serde_json::from_value(serde_json::json!({ "name": "probe.O", "properties": properties }))
            .expect("probe type"),
    );
    let defs = norn_expr::EntityDefinitions {
        property_types: &model.property_types,
        interfaces: &model.interfaces,
        object_types: &object_types,
        extensions: &model.extensions,
    };
    ZenTypeEnv::from_artifact_set(&defs, EnvOptions::default())
        .expect("env")
        .bind_input(fields([
            ("a", Type::Number),
            ("b", Type::Number),
            ("amount", Type::Number),
            ("s", Type::text()),
            ("flag", Type::Bool),
            ("date_a", Type::Date),
            ("date_b", Type::Date),
            (
                "timestamp_utc",
                Type::Timestamp {
                    timezone: TimezonePolicy::Utc,
                },
            ),
            (
                "timestamp_offset",
                Type::Timestamp {
                    timezone: TimezonePolicy::Offset,
                },
            ),
            ("o", Type::Record("probe.O".into())),
            ("xs", Type::Number.list()),
        ]))
        .expect("input")
}

/// The checker's verdict: `Ok` or the rejection codes (a source the engine
/// cannot even compile is rejected as `expr.syntax.invalid`).
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
fn admitted(source: &str, expected: &Type) {
    if let Err(codes) = verdict(source, expected) {
        panic!("`{source}` is in {CHECKER_PROFILE} but was rejected: {codes:?}");
    }
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

const OUTSIDE: &[&str] = &[codes::UNSUPPORTED_CONSTRUCT, codes::SYNTAX];

#[test]
fn profile_id_is_published_and_pinned_to_profile_md() {
    assert_eq!(CHECKER_PROFILE, "norn-zen/6");
    let text = profile_document();
    assert!(text.contains("`norn-zen/6`"), "PROFILE.md must name the id");
    let hash = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(text.as_bytes());
    assert_eq!(
        hash, PROFILE_MD_HASH,
        "PROFILE.md changed: bump CHECKER_PROFILE if a rule changed, then re-pin"
    );
}

#[test]
fn profile_engine_pin_matches_the_qualified_fork() {
    let pin: serde_json::Value =
        serde_json::from_str(include_str!("../../norn-skeleton-guard/zen-pin.json")).unwrap();
    assert_eq!(pin["fork"]["tag"], "norn-v2.0.1-12");
    assert_eq!(
        pin["fork"]["commit"],
        "44530255a36df637edc86f6d29495c342805e21b"
    );
    assert!(profile_document().contains("`norn-v2.0.1-12`"));
}

#[test]
fn builtins_admitted_and_refused() {
    for source in [
        "len(input.s)",
        "len(input.xs)",
        "split(input.s, '')",
        "len(split(input.s, '')) - 2",
        "lower(input.s) + upper(input.s)",
        "trim(input.s)",
        "contains(input.s, 'a')",
        "startsWith(input.s, 'a')",
        "endsWith(input.s, 'a')",
        "tokens(input.s, 'alnum', 24, 32)",
        "take(input.s, 128)",
        "join(tokens(input.s, 'alnum', 24, 32), '-')",
        "take(upper(join(tokens(input.s, 'alnum', 24, 32), '-')), 128)",
        "join([], '-')",
        "abs(input.a) + floor(input.a) + ceil(input.b)",
        "round(input.a, 2) + trunc(input.b, 0)",
        "sum(input.xs)",
        "count(input.xs, # > 1)",
        "some(input.xs, # > 1)",
        "all(input.xs, # > 1)",
        "none(input.xs, # > 1)",
        "one(input.xs, # > 1)",
        "filter(input.xs, # > 1)",
        "map(input.xs, # + 1)",
        "flatMap(input.xs, [#, # + 1])",
        "date(input.date_a)",
        "timestamp(input.timestamp_utc)",
        "days_between(date(input.date_a), date(input.date_b))",
        "add_days(date(input.date_a), 1)",
        "seconds_between(timestamp(input.timestamp_utc), timestamp(input.timestamp_offset))",
        "add_seconds(timestamp(input.timestamp_utc), 1)",
        "add_days(date(input.date_a), input.amount)",
        "add_seconds(timestamp(input.timestamp_utc), input.amount)",
    ] {
        admitted(source, &Type::Json);
    }
    for source in [
        "split(input.s, ',')",
        "split(input.s, input.s)",
        "tokens(input.s, input.s, 24, 32)",
        "tokens(input.s, 'alnum', input.a, 32)",
        "tokens(input.s, 'alnum', 257, 32)",
        "take(input.s, 16385)",
        "join(['a'], input.s)",
        "rand(10)",
        "string(input.a)",
        "number(input.s)",
        "values(input.o)",
        "merge([input.o])",
        "max(input.xs)",
        "min(input.xs)",
        "avg(input.xs)",
        "input.a % 2",
        "input.a ^ 2",
        "`${input.s}`",
        "input.s[0:2]",
    ] {
        refused(source, &Type::Json, OUTSIDE);
    }
    refused(
        "join([input.date_a], '-')",
        &Type::Json,
        &[codes::TYPE_ERROR],
    );
    for source in [
        "len(input.a)",
        "split(input.a, '')",
        "lower(input.a)",
        "upper(input.a)",
        "trim(input.a)",
        "contains(input.a, 'a')",
        "startsWith(input.a, 'a')",
        "endsWith(input.a, 'a')",
        "abs(input.s)",
        "floor(input.s)",
        "ceil(input.s)",
        "round(input.a, 29)",
        "trunc(input.a, input.b)",
        "sum(input.s)",
        "count(input.xs, #)",
        "some(input.xs, #)",
        "all(input.xs, #)",
        "none(input.xs, #)",
        "one(input.xs, #)",
        "filter(input.xs, #)",
        "map(input.xs, # / 0)",
        "flatMap(input.xs, #)",
    ] {
        refused(source, &Type::Json, &[codes::TYPE_ERROR, codes::NULLABLE]);
    }
}

#[test]
fn bounded_string_controls_are_checked_at_typed_and_syntax_only_sites() {
    for source in [
        "tokens(input.s, input.s, 1, 1)",
        "tokens(input.s, 'alnum', 257, 1)",
        "take(input.s, input.a)",
        "take(input.s, 16385)",
        "join(['x'], input.s)",
    ] {
        assert!(
            validate_evaluation_source(source, &ResourceLimitProfile::norn_limits_v1()).is_err(),
            "syntax-only admission accepted `{source}`"
        );
    }
    let oversized_separator = format!("join(['x'], '{}')", "-".repeat(257));
    assert!(
        validate_evaluation_source(
            &oversized_separator,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .is_err(),
        "syntax-only admission accepted an oversized separator"
    );
    assert!(
        validate_unary_evaluation_source(
            "take($, input.a) == 'x'",
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .is_err(),
        "unary syntax-only admission accepted a dynamic cap"
    );
    validate_evaluation_source(
        "take(input.s, 16384)",
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .expect("the inclusive upper cap is admitted");
    validate_evaluation_source("join([], '-')", &ResourceLimitProfile::norn_limits_v1())
        .expect("an empty literal string list is admitted with the builtin's expected type");
}

#[test]
fn bounded_strings_follow_nfc_and_unicode_scalar_boundaries() {
    let input = Value::object([("s", Value::from("cafe\u{301}😀code"))]);
    assert_eq!(
        eval("tokens(input.s, 'alnum', 8, 32)", input.clone()).unwrap(),
        Value::Array(vec![Value::from("caf"), Value::from("code")]),
    );
    assert_eq!(
        eval("take(input.s, 5)", input).unwrap(),
        Value::from("café😀"),
    );
}

#[test]
fn bounded_string_zero_caps_return_empty_results() {
    let input = Value::object([("s", Value::from("abc-123"))]);
    assert_eq!(
        eval("tokens(input.s, 'alnum', 0, 4)", input.clone()).unwrap(),
        Value::Array(Vec::new()),
    );
    assert_eq!(
        eval("tokens(input.s, 'alnum', 8, 0)", input).unwrap(),
        Value::Array(Vec::new()),
    );
}

#[test]
fn bounded_string_precharge_exhaustion_is_typed() {
    let expression = Expression::compile("tokens(input.s, 'alnum', 24, 32)").unwrap();
    let input = Value::object([("input", Value::object([("s", Value::from("abc-123"))]))]);
    let attempt = expression.evaluate_metered(&input, EvaluationBudget::new(10));
    assert!(matches!(
        attempt.result,
        Err(ExprError::BudgetExhausted { limit: 10, .. })
    ));
    assert!(attempt.units_used <= 10);
}

#[test]
fn ambient_clock_and_timezone_forms_are_refused() {
    for source in [
        "d()",
        "now()",
        "today()",
        "yesterday()",
        "d('2024-01-01', 'UTC')",
        "d('2026-03-29 02:30:00', 'Europe/Berlin').hour()",
        "d('2024-02-29', 'UTC').add('1y').year()",
    ] {
        refused(source, &Type::Json, OUTSIDE);
    }
}

#[test]
fn temporal_kinds_separate_wire_strings_from_parsed_temporals() {
    for source in [
        "input.date_a == input.date_b",
        "input.timestamp_utc == input.timestamp_offset",
        "date(input.date_a) == date(input.date_b)",
        "timestamp(input.timestamp_utc) == timestamp(input.timestamp_offset)",
    ] {
        admitted(source, &Type::Bool);
    }
    for operator in ["<", "<=", ">", ">="] {
        admitted(
            &format!("date(input.date_a) {operator} date(input.date_b)"),
            &Type::Bool,
        );
        admitted(
            &format!("timestamp(input.timestamp_utc) {operator} timestamp(input.timestamp_offset)"),
            &Type::Bool,
        );
    }
    for source in [
        "input.date_a == input.s",
        "input.date_a == input.timestamp_utc",
        "input.date_a < input.date_b",
        "input.timestamp_utc >= input.timestamp_offset",
        "date(input.date_a) < timestamp(input.timestamp_utc)",
        "date(input.date_a) == input.date_b",
        "input.date_b == date(input.date_a)",
        "timestamp(input.timestamp_utc) == input.timestamp_offset",
        "input.timestamp_offset == timestamp(input.timestamp_utc)",
        "date(input.timestamp_utc)",
        "timestamp(input.date_a)",
        "input.date_a + ''",
        "len(input.timestamp_utc)",
        "input.date_a in [input.date_b]",
        "input.flag ? input.date_a : input.s",
        "[input.date_a, input.s]",
        "([input.date_a, input.date_b])[0] == input.s",
        "days_between(date(input.date_a), timestamp(input.timestamp_utc))",
        "add_days(date(input.date_a), 1.5)",
        "add_seconds(timestamp(input.timestamp_utc), -0.25)",
        "date('2026-02-29')",
        "date('1900-02-29')",
        "timestamp('2026-01-01T00:00:00')",
    ] {
        refused(
            source,
            &Type::Bool,
            &[codes::TYPE_ERROR, codes::RESULT_MISMATCH],
        );
    }
    refused("date()", &Type::Json, &[codes::SYNTAX]);
    refused("timestamp()", &Type::Json, &[codes::SYNTAX]);
    for source in [
        "date('0000-01-01')",
        "date('0000-02-29')",
        "date('1900-02-29')",
        "date('2026-02-29')",
        "timestamp('2026-02-29T00:00:00Z')",
        "timestamp('0000-02-29T00:00:00Z')",
        "timestamp('2026-01-01T00:00:00')",
        "timestamp('2026-01-01T00:00:00-00:00')",
        "timestamp('2026-01-01T00:00:60Z')",
        "timestamp('2026-01-01T00:00:00.1234567890Z')",
        "timestamp('0001-01-01T00:00:00+00:01')",
        "timestamp('9999-12-31T23:59:59-00:01')",
    ] {
        refused(source, &Type::Bool, &[codes::TYPE_ERROR]);
    }
    refused("'2026-01-01'", &Type::Date, &[codes::RESULT_MISMATCH]);
    refused(
        "'2026-01-01T00:00:00Z'",
        &Type::Timestamp {
            timezone: TimezonePolicy::Utc,
        },
        &[codes::RESULT_MISMATCH],
    );

    assert_eq!(
        eval(
            "input.timestamp_utc == input.timestamp_offset",
            Value::object([
                (
                    "timestamp_utc",
                    Value::from(
                        norn_expr::temporal::canonical_timestamp("2026-01-01T02:00:00+02:00")
                            .expect("canonical UTC timestamp"),
                    ),
                ),
                ("timestamp_offset", Value::from("2026-01-01T00:00:00Z")),
            ]),
        )
        .expect("evaluate canonical temporal equality"),
        Value::Bool(true)
    );
}

fn typed_evaluation_error(source: &str, input: Value) {
    match eval(source, input) {
        Err(ExprError::Evaluation { .. }) => {}
        other => panic!("`{source}` should fail with a typed evaluation error, got {other:?}"),
    }
}

#[test]
fn temporal_ordering_handles_season_windows_leap_days_and_offsets() {
    let season = Value::object([
        ("seasonStart", Value::from("2024-02-29")),
        ("seasonEnd", Value::from("2024-03-01")),
    ]);
    assert_eq!(
        eval(
            "date(input.seasonEnd) >= date(input.seasonStart)",
            season.clone()
        )
        .expect("season end follows season start"),
        Value::Bool(true)
    );
    assert_eq!(
        eval("date(input.seasonStart) < date(input.seasonEnd)", season)
            .expect("date ordering uses calendar days"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(
            "date(input.seasonEnd) >= date(input.seasonStart)",
            Value::object([
                ("seasonStart", Value::from("2024-03-01")),
                ("seasonEnd", Value::from("2024-02-29")),
            ])
        )
        .expect("reversed season window is false"),
        Value::Bool(false)
    );
    for (operator, expected) in [("<", true), ("<=", true), (">", false), (">=", false)] {
        assert_eq!(
            eval(
                &format!("date('2024-02-28') {operator} date('2024-02-29')"),
                Value::object::<String>([])
            )
            .expect("date comparison"),
            Value::Bool(expected),
            "date operator {operator}"
        );
    }
    assert_eq!(
        eval(
            "date('1900-02-28') < date('1900-03-01') and date('2000-02-29') < date('2000-03-01')",
            Value::object::<String>([])
        )
        .expect("Gregorian leap boundaries"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(
            "timestamp('2026-01-01T02:00:00+02:00') == timestamp('2026-01-01T00:00:00Z')",
            Value::object::<String>([])
        )
        .expect("equivalent offsets compare as one instant"),
        Value::Bool(true)
    );
    assert_eq!(
        eval(
            "timestamp('2026-01-01T00:00:00.123456789Z') > timestamp('2026-01-01T00:00:00.123456788Z')",
            Value::object::<String>([])
        )
        .expect("fractional timestamps retain nanosecond ordering"),
        Value::Bool(true)
    );
    for (operator, expected) in [("<", true), ("<=", true), (">", false), (">=", false)] {
        assert_eq!(
            eval(
                &format!("timestamp('2026-01-01T00:00:00Z') {operator} timestamp('2026-01-01T00:00:00.000000001Z')"),
                Value::object::<String>([])
            )
            .expect("timestamp comparison"),
            Value::Bool(expected),
            "timestamp operator {operator}"
        );
    }
}

#[test]
fn temporal_arithmetic_is_checked_and_projects_canonical_strings() {
    assert_eq!(
        eval(
            "add_days(date(input.start), 1)",
            Value::object([("start", Value::from("2024-02-29"))])
        )
        .expect("add one day across leap boundary"),
        Value::from("2024-03-01")
    );
    assert_eq!(
        eval(
            "days_between(date('2024-02-28'), date('2024-03-01'))",
            Value::object::<String>([])
        )
        .expect("signed calendar-day difference"),
        num("2")
    );
    assert_eq!(
        eval(
            "seconds_between(timestamp('2026-01-01T00:00:00.125Z'), timestamp('2026-01-01T00:00:01.375Z'))",
            Value::object::<String>([])
        )
        .expect("exact fractional seconds"),
        num("1.25")
    );
    assert_eq!(
        eval(
            "add_seconds(timestamp('2026-01-01T00:00:00.125Z'), 2)",
            Value::object::<String>([])
        )
        .expect("preserve fractional timestamp precision"),
        Value::from("2026-01-01T00:00:02.125Z")
    );
    for invalid in ["1900-02-29", "0000-01-01", "2026-02-29"] {
        typed_evaluation_error(
            "date(input.s)",
            Value::object([("s", Value::from(invalid))]),
        );
    }
    typed_evaluation_error(
        "date(input.date_a)",
        Value::object([("date_a", Value::from("0000-02-29"))]),
    );
    typed_evaluation_error(
        "timestamp(input.timestamp_utc)",
        Value::object([("timestamp_utc", Value::from("0000-02-29T00:00:00Z"))]),
    );
    for invalid in [
        "2026-01-01T00:00:00",
        "2026-01-01T00:00:00-00:00",
        "2026-01-01T00:00:60Z",
        "2026-01-01T00:00:00.1234567890Z",
        "0001-01-01T00:00:00+00:01",
        "9999-12-31T23:59:59-00:01",
    ] {
        typed_evaluation_error(
            "timestamp(input.s)",
            Value::object([("s", Value::from(invalid))]),
        );
    }
    typed_evaluation_error(
        "add_days(date('0001-01-01'), -1)",
        Value::object::<String>([]),
    );
    typed_evaluation_error(
        "add_seconds(timestamp('0001-01-01T00:00:00Z'), -1)",
        Value::object::<String>([]),
    );
    typed_evaluation_error(
        "add_days(date('9999-12-31'), 1)",
        Value::object::<String>([]),
    );
    typed_evaluation_error(
        "add_seconds(timestamp('9999-12-31T23:59:59Z'), 1)",
        Value::object::<String>([]),
    );
    typed_evaluation_error(
        "add_days(date(input.start), input.amount)",
        Value::object([("start", Value::from("2024-02-29")), ("amount", num("0.5"))]),
    );
    typed_evaluation_error(
        "add_seconds(timestamp(input.start), input.amount)",
        Value::object([
            ("start", Value::from("2026-01-01T00:00:00Z")),
            ("amount", num("0.5")),
        ]),
    );
    let overflowing_amount = num("79228162514264337593543950335");
    typed_evaluation_error(
        "add_days(date(input.start), input.amount)",
        Value::object([
            ("start", Value::from("2024-02-29")),
            ("amount", overflowing_amount.clone()),
        ]),
    );
    typed_evaluation_error(
        "add_seconds(timestamp(input.start), input.amount)",
        Value::object([
            ("start", Value::from("2026-01-01T00:00:00Z")),
            ("amount", overflowing_amount),
        ]),
    );
}

#[test]
fn types_have_no_residual_any_or_dynamic_reads() {
    admitted("input.o.x", &Type::text());
    admitted("input.o['x']", &Type::text());
    // Mandatory F2 counterexample: upstream strict analysis types this as
    // Number with no diagnostics.
    refused(
        "input.o[input.s] - 1",
        &Type::Number,
        &[codes::DYNAMIC_READ],
    );
    refused("input.o['x'] - 1", &Type::Number, &[codes::TYPE_ERROR]);
    refused(
        "input.nope",
        &Type::Json,
        &[codes::UNKNOWN_MEMBER, codes::UNKNOWN_VARIABLE],
    );
}

#[test]
fn map_iteration_order_is_never_observable() {
    // Mandatory counterexample: `keys` over a 40-key record (eight observed
    // orders upstream before deterministic-maps) is outside the profile.
    refused("keys(input.o)", &Type::Json, OUTSIDE);
    refused("map(keys(input.o), #)", &Type::Json, OUTSIDE);
    // Lists keep their order.
    admitted("map(input.xs, # * 2)", &Type::Number.list());
    assert_eq!(
        eval(
            "map(input.xs, # * 2)",
            Value::object([("xs", Value::Array(vec![num("3"), num("1"), num("2")]))])
        )
        .unwrap(),
        Value::Array(vec![num("6"), num("2"), num("4")])
    );
}

#[test]
fn numbers_are_exact_or_a_typed_error() {
    admitted("input.a + input.b", &Type::Number);
    let sum = |a: &str, b: &str| {
        eval(
            "input.a + input.b",
            Value::object([("a", num(a)), ("b", num(b))]),
        )
    };
    // Mandatory: 2^53 + 1 round-trips exactly.
    assert_eq!(
        sum("9007199254740993", "0").unwrap(),
        num("9007199254740993")
    );
    assert_eq!(
        eval("9007199254740993", Value::Null).unwrap(),
        num("9007199254740993")
    );
    assert_eq!(sum("0.1", "0.2").unwrap(), num("0.3"));
    // Checked decimal arithmetic can reduce scale. Ties round to even;
    // explicit round() below instead rounds half away from zero.
    let tiny = "0.0000000000000000000000000001";
    for (factor, expected) in [
        ("0.5", "0"),
        ("1.5", "0.0000000000000000000000000002"),
        ("2.5", "0.0000000000000000000000000002"),
    ] {
        assert_eq!(
            eval(
                "input.a * input.b",
                Value::object([("a", num(tiny)), ("b", num(factor))]),
            )
            .unwrap(),
            num(expected)
        );
    }
    // Beyond the 96-bit decimal: rejected as a value, never rounded.
    assert!(matches!(
        "79228162514264337593543950336".parse::<Number>(),
        Err(ExprError::InvalidNumber { .. })
    ));
    // Division rounds to the engine's 28 fractional digits (pinned).
    assert_eq!(
        eval(
            "input.a / input.b",
            Value::object([("a", num("1")), ("b", num("3"))])
        )
        .unwrap(),
        num("0.3333333333333333333333333333")
    );
    // Rounding mode (pinned).
    assert_eq!(eval("round(2.5, 0)", Value::Null).unwrap(), num("3"));
    assert_eq!(eval("round(-2.5, 0)", Value::Null).unwrap(), num("-3"));
    assert_eq!(eval("trunc(2.567, 2)", Value::Null).unwrap(), num("2.56"));
}

#[test]
fn scientific_literals_are_refused_before_evaluation() {
    for source in [
        "0.00000000000000000000000000001e0",
        "0.00000000000000000000000000001E0",
        "1e0",
        "1e-1",
        "1e+1",
        "input.a + 0.00000000000000000000000000001e0",
        "{amount: 0.00000000000000000000000000001e0}",
    ] {
        refused(source, &Type::Json, OUTSIDE);
    }
    // The same source-literal check covers decision-table unary cells.
    let unary = UnaryTest::compile(">= 0.00000000000000000000000000001e0").unwrap();
    let findings = check_unary_with(&unary, &probe_env(), &Type::Number, &roomy_limits())
        .expect_err("inexact scientific unary cell must not be admitted");
    assert!(
        findings
            .iter()
            .any(|d| d.code == codes::UNSUPPORTED_CONSTRUCT)
    );
    // A spelling inside text is not a numeric token.
    admitted("'1e0'", &Type::text());
    admitted("0.0000000000000000000000000001", &Type::Number);
}

#[test]
fn evaluation_source_validation_reuses_exact_literal_and_structural_bounds() {
    let limits = ResourceLimitProfile::norn_limits_v1();
    assert!(
        validate_evaluation_source(
            "input.record.amount + 0.0000000000000000000000000001",
            &limits
        )
        .is_ok()
    );

    for source in [
        "1e0",
        "input.record.amount + 0.00000000000000000000000000001e0",
    ] {
        let diagnostics = validate_evaluation_source(source, &limits)
            .expect_err("inexact numeric source must be rejected before evaluation");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == codes::UNSUPPORTED_CONSTRUCT),
            "{source}: {diagnostics:?}"
        );
    }

    // The engine parser rejects an overflowing mantissa before exposing an AST.
    let diagnostics = validate_evaluation_source("79228162514264337593543950336", &limits)
        .expect_err("overflowing mantissa must be rejected");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == codes::SYNTAX)
    );

    assert!(validate_evaluation_source("input.record.amount +", &limits).is_err());
    assert!(validate_evaluation_source(&"x".repeat(16_385), &limits).is_err());
}

#[test]
fn nullable_division_requires_handling() {
    let optional_number = Type::Number.optional();
    admitted("input.a / input.b", &optional_number);
    admitted("(input.a / input.b) ?? 0", &Type::Number);
    admitted("input.a / 2", &Type::Number);
    for source in ["input.a / input.b", "input.a / 0", "input.a / 0.1"] {
        refused(source, &Type::Number, &[codes::RESULT_MISMATCH]);
    }
    refused("input.a / input.b + 1", &Type::Number, &[codes::NULLABLE]);
    let div = eval(
        "input.a / input.b",
        Value::object([("a", num("1")), ("b", num("0"))]),
    );
    assert_eq!(div.unwrap(), Value::Null);
    assert_eq!(
        eval(
            "input.a / 0.1",
            Value::object([("a", num("79228162514264337593543950335"))]),
        )
        .unwrap(),
        Value::Null
    );
}

#[test]
fn runtime_errors_are_typed_never_silent() {
    let big = "79228162514264337593543950335";
    let overflow = eval(
        "input.a * input.b",
        Value::object([("a", num(big)), ("b", num(big))]),
    );
    assert!(
        matches!(overflow, Err(ExprError::Evaluation { .. })),
        "{overflow:?}"
    );
    // Budget exhaustion is typed too.
    let exhausted = Expression::compile("map([0..1000], # * 2)")
        .unwrap()
        .evaluate(&Value::Null, EvaluationBudget::new(10));
    assert!(
        matches!(exhausted, Err(ExprError::BudgetExhausted { .. })),
        "{exhausted:?}"
    );
}

#[test]
fn regex_is_refused_and_strings_are_bounded() {
    refused("matches(input.s, '^a')", &Type::Json, OUTSIDE);
    refused("extract(input.s, '(a)')", &Type::Json, OUTSIDE);
    admitted("startsWith(input.s, 'a')", &Type::Bool);
    let limit = StructuralLimits::NORN_EXPR_4.max_source_bytes;
    let long = format!("'{}'", "a".repeat(usize::try_from(limit).unwrap()));
    match Expression::compile(&long) {
        Err(ExprError::Compile { message }) => assert!(message.contains(codes::BOUND_SOURCE)),
        other => panic!("expected source-bound refusal, got {other:?}"),
    }
    let literal_limit = StructuralLimits::NORN_EXPR_4.max_string_literal_bytes;
    let long_literal = format!(
        "'{}'",
        "a".repeat(usize::try_from(literal_limit).unwrap() + 1)
    );
    refused(&long_literal, &Type::text(), &[codes::BOUND_LITERAL]);
}

#[test]
fn unicode_scalar_count_preserves_byte_length_semantics() {
    for text in ["", "ع", "😀", "ع😀", "e\u{301}"] {
        let normalized = if text == "e\u{301}" { "é" } else { text };
        let input = Value::object([("s", Value::from(text))]);
        assert_eq!(
            eval("len(input.s)", input.clone()).unwrap(),
            Value::from(normalized.len() as i64)
        );
        assert_eq!(
            eval("len(split(input.s, '')) - 2", input).unwrap(),
            Value::from(normalized.chars().count() as i64)
        );
    }
}

fn profile_document() -> &'static str {
    include_str!("../README.md")
        .split_once("<!-- profile:start -->\n")
        .expect("profile start")
        .1
        .split_once("<!-- profile:end -->")
        .expect("profile end")
        .0
}
