//! Norn temporal qualification fixtures. CI runs this same suite natively and
//! under wasm-bindgen-test on wasm32-unknown-unknown.

use serde_json::json;
use zen_expression::{Isolate, Variable};

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

fn evaluate(expression: &str) -> Variable {
    Isolate::new()
        .run_standard(expression)
        .unwrap_or_else(|error| panic!("{expression}: {error:?}"))
}

fn boolean(expression: &str, expected: bool) {
    assert_eq!(
        evaluate(expression).as_bool(),
        Some(expected),
        "{expression}"
    );
}

fn number(expression: &str, expected: &str) {
    assert_eq!(
        evaluate(expression)
            .as_number()
            .unwrap()
            .normalize()
            .to_string(),
        expected,
        "{expression}"
    );
}

fn text(expression: &str, expected: &str) {
    assert_eq!(
        evaluate(expression).as_str(),
        Some(expected),
        "{expression}"
    );
}

fn rejected(expression: &str) {
    assert!(
        Isolate::new().run_standard(expression).is_err(),
        "must reject {expression}"
    );
}

#[test]
fn gregorian_leap_boundaries_and_signed_day_arithmetic() {
    for (year, days) in [(1900, "1"), (2000, "2"), (2100, "1"), (2024, "2")] {
        number(
            &format!("days_between(date('{year}-02-28'), date('{year}-03-01'))"),
            days,
        );
        number(
            &format!("days_between(date('{year}-03-01'), date('{year}-02-28'))"),
            &format!("-{days}"),
        );
    }
    for expression in ["date('1900-02-29')", "date('2100-02-29')"] {
        rejected(expression);
    }
    text("string(date('2000-02-29'))", "2000-02-29");
    text("string(add_days(date('2000-02-28'), 1))", "2000-02-29");
    text("string(add_days(date('2000-02-28'), 2))", "2000-03-01");
    text("string(add_days(date('1900-02-28'), 1))", "1900-03-01");
    text("string(add_days(date('2100-03-01'), -1))", "2100-02-28");
    text("string(add_days(date('2024-12-31'), 1))", "2025-01-01");
    text("string(add_days(date('2025-01-01'), -1))", "2024-12-31");
    number("days_between(date('2024-02-29'), date('2024-02-29'))", "0");
}

#[test]
fn all_same_kind_relational_operators_form_a_total_order() {
    for (before, equal, after) in [
        (
            "date('2024-02-28')",
            "date('2024-02-28')",
            "date('2024-02-29')",
        ),
        (
            "timestamp('2024-02-29T23:59:59.999999998Z')",
            "timestamp('2024-02-29T23:59:59.999999998+00:00')",
            "timestamp('2024-02-29T23:59:59.999999999Z')",
        ),
    ] {
        for (operator, less, same, greater) in [
            ("<", true, false, false),
            ("<=", true, true, false),
            (">", false, false, true),
            (">=", false, true, true),
            ("==", false, true, false),
            ("!=", true, false, true),
        ] {
            boolean(&format!("{before} {operator} {after}"), less);
            boolean(&format!("{before} {operator} {equal}"), same);
            boolean(&format!("{after} {operator} {before}"), greater);
        }
    }
}

#[test]
fn season_bounds_parse_json_property_strings_explicitly() {
    // The platform stores semantic properties as JSON strings. The fork seam
    // parses each declared kind explicitly before ordering it.
    let mut isolate = Isolate::with_environment(
        json!({
            "season_start": "2024-03-01",
            "season_end": "2024-06-01",
            "instant_start": "2024-03-01T00:00:00Z",
            "instant_end": "2024-06-01T00:00:00Z"
        })
        .into(),
    );
    for (point, included) in [
        ("2024-02-29", false),
        ("2024-03-01", true),
        ("2024-05-31", true),
        ("2024-06-01", false),
    ] {
        let expression =
            format!("date(season_start) <= date('{point}') and date('{point}') < date(season_end)");
        assert_eq!(
            isolate.run_standard(&expression).unwrap().as_bool(),
            Some(included)
        );
    }
    for (point, included) in [
        ("2024-02-29T23:59:59.999999999Z", false),
        ("2024-03-01T01:00:00+01:00", true),
        ("2024-05-31T23:59:59.999999999Z", true),
        ("2024-06-01T00:00:00Z", false),
    ] {
        let expression = format!(
            "timestamp(instant_start) <= timestamp('{point}') and timestamp('{point}') < timestamp(instant_end)"
        );
        assert_eq!(
            isolate.run_standard(&expression).unwrap().as_bool(),
            Some(included)
        );
    }
}

#[test]
fn equivalent_offsets_compare_and_serialize_as_the_same_instant() {
    let utc = "timestamp('2024-02-29T23:30:00.123456789Z')";
    for input in [
        "2024-03-01T00:30:00.123456789+01:00",
        "2024-02-29T18:00:00.123456789-05:30",
        "2024-03-01T13:30:00.123456789+14:00",
        "2024-02-29T23:30:00.123456789+00:00",
    ] {
        let value = format!("timestamp('{input}')");
        boolean(&format!("{utc} == {value}"), true);
        number(&format!("seconds_between({utc}, {value})"), "0");
        text(
            &format!("string({value})"),
            "2024-02-29T23:30:00.123456789Z",
        );
        assert_eq!(
            serde_json::to_value(evaluate(&value)).unwrap(),
            json!("2024-02-29T23:30:00.123456789Z")
        );
    }
    assert_eq!(
        serde_json::to_value(evaluate("date('2024-02-29')")).unwrap(),
        json!("2024-02-29")
    );
}

#[test]
fn fractional_seconds_preserve_each_supported_precision() {
    for (fraction, canonical, seconds) in [
        ("1", "100", "0.1"),
        ("12", "120", "0.12"),
        ("123", "123", "0.123"),
        ("1234", "123400", "0.1234"),
        ("12345", "123450", "0.12345"),
        ("123456", "123456", "0.123456"),
        ("1234567", "123456700", "0.1234567"),
        ("12345678", "123456780", "0.12345678"),
        ("123456789", "123456789", "0.123456789"),
    ] {
        let value = format!("timestamp('2000-02-29T12:34:56.{fraction}Z')");
        text(
            &format!("string({value})"),
            &format!("2000-02-29T12:34:56.{canonical}Z"),
        );
        number(
            &format!("seconds_between(timestamp('2000-02-29T12:34:56Z'), {value})"),
            seconds,
        );
        number(
            &format!("seconds_between({value}, timestamp('2000-02-29T12:34:56Z'))"),
            &format!("-{seconds}"),
        );
        text(
            &format!("string(add_seconds({value}, 1))"),
            &format!("2000-02-29T12:34:57.{canonical}Z"),
        );
    }
    text(
        "string(timestamp('2000-02-29T12:34:56.000000000Z'))",
        "2000-02-29T12:34:56Z",
    );
    number(
        "seconds_between(timestamp('2024-03-01T00:00:00Z'), timestamp('2024-02-29T23:59:59.999999999Z'))",
        "-0.000000001",
    );
    text(
        "string(add_seconds(timestamp('2024-02-29T23:59:59.999999999Z'), 1))",
        "2024-03-01T00:00:00.999999999Z",
    );
    text(
        "string(add_seconds(timestamp('2024-03-01T00:00:00Z'), -1))",
        "2024-02-29T23:59:59Z",
    );
}

#[test]
fn date_range_is_closed_and_overflow_is_an_error() {
    text("string(add_days(date('0001-01-01'), 0))", "0001-01-01");
    text("string(add_days(date('9999-12-31'), 0))", "9999-12-31");
    number(
        "days_between(date('0001-01-01'), date('9999-12-31'))",
        "3652058",
    );
    text(
        "string(add_days(date('0001-01-01'), 3652058))",
        "9999-12-31",
    );
    text(
        "string(add_days(date('9999-12-31'), -3652058))",
        "0001-01-01",
    );
    text(
        "string(add_seconds(timestamp('0001-01-01T00:00:00Z'), 0))",
        "0001-01-01T00:00:00Z",
    );
    text(
        "string(add_seconds(timestamp('9999-12-31T23:59:59.999999999Z'), 0))",
        "9999-12-31T23:59:59.999999999Z",
    );
    // The full supported range exceeds i64 nanoseconds. Differences must stay
    // exact rather than overflowing a nanosecond-duration intermediate.
    number(
        "seconds_between(timestamp('0001-01-01T00:00:00Z'), timestamp('9999-12-31T23:59:59.999999999Z'))",
        "315537897599.999999999",
    );
    number(
        "seconds_between(timestamp('9999-12-31T23:59:59.999999999Z'), timestamp('0001-01-01T00:00:00Z'))",
        "-315537897599.999999999",
    );
    for expression in [
        "add_days(date('0001-01-01'), -1)",
        "add_days(date('9999-12-31'), 1)",
        "add_days(date('2024-02-29'), 9223372036854775807)",
        "add_days(date('2024-02-29'), -9223372036854775808)",
        "add_days(date('2024-02-29'), 79228162514264337593543950335)",
        "add_seconds(timestamp('0001-01-01T00:00:00Z'), -1)",
        "add_seconds(timestamp('9999-12-31T23:59:59.999999999Z'), 1)",
        "add_seconds(timestamp('2024-02-29T00:00:00Z'), 9223372036854775807)",
        "add_seconds(timestamp('2024-02-29T00:00:00Z'), -9223372036854775808)",
        "add_seconds(timestamp('2024-02-29T00:00:00Z'), 79228162514264337593543950335)",
    ] {
        rejected(expression);
    }
}

#[test]
fn invalid_calendars_and_noncanonical_date_grammar_are_rejected() {
    for input in [
        "2024-02-30",
        "2023-02-29",
        "2000-04-31",
        "0000-01-01",
        "10000-01-01",
        "-0001-01-01",
        "2024-00-01",
        "2024-13-01",
        "2024-01-00",
        "2024-01-32",
        "2024-2-29",
        "24-02-29",
        "2024/02/29",
        "2024",
        "2024-02",
        "",
        " 2024-02-29",
        "2024-02-29 ",
        "2024-02-29T00:00:00Z",
        "today",
        "now",
    ] {
        rejected(&format!("date('{input}')"));
    }
}

#[test]
fn timestamps_require_valid_calendar_supported_precision_and_explicit_offset() {
    for input in [
        "2024-02-30T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2100-02-29T00:00:00Z",
        "0000-01-01T00:00:00Z",
        "10000-01-01T00:00:00Z",
        "2024-02-29",
        "2024-02-29T12:34:56",
        "2024-02-29 12:34:56Z",
        "2024-02-29t12:34:56Z",
        "2024-02-29T12:34:56z",
        "2024-02-29T12:34Z",
        "2024-02-29T24:00:00Z",
        "2024-02-29T12:60:00Z",
        "2024-02-29T12:34:60Z",
        "2016-12-31T23:59:60Z",
        "2024-02-29T12:34:56.Z",
        "2024-02-29T12:34:56.1234567890Z",
        "2024-02-29T12:34:56,123Z",
        "2024-02-29T12:34:56+24:00",
        "2024-02-29T12:34:56+00:60",
        "2024-02-29T12:34:56+0000",
        "2024-02-29T12:34:56+00",
        "2024-02-29T12:34:56-00:00",
        "2024-02-29T12:34:56 UTC",
        "0001-01-01T00:00:00+00:01",
        "9999-12-31T23:59:59-00:01",
        " 2024-02-29T12:34:56Z",
        "2024-02-29T12:34:56Z ",
        "",
        "now",
        "today",
    ] {
        rejected(&format!("timestamp('{input}')"));
    }
}

#[test]
fn mixed_kinds_and_implicit_string_or_number_coercion_are_rejected() {
    let date = "date('2024-02-29')";
    let timestamp = "timestamp('2024-02-29T00:00:00Z')";
    for operator in ["<", "<=", ">", ">=", "==", "!="] {
        for (left, right) in [
            (date, timestamp),
            (timestamp, date),
            (date, "'2024-02-29'"),
            ("'2024-02-29'", date),
            (timestamp, "'2024-02-29T00:00:00Z'"),
            ("'2024-02-29T00:00:00Z'", timestamp),
            (date, "0"),
            ("0", timestamp),
            (date, "null"),
            (timestamp, "false"),
        ] {
            rejected(&format!("{left} {operator} {right}"));
        }
    }
    for expression in [
        "days_between(date('2024-02-29'), timestamp('2024-02-29T00:00:00Z'))",
        "seconds_between(timestamp('2024-02-29T00:00:00Z'), date('2024-02-29'))",
        "days_between('2024-02-28', '2024-02-29')",
        "seconds_between('2024-02-29T00:00:00Z', '2024-02-29T00:00:01Z')",
        "add_days(timestamp('2024-02-29T00:00:00Z'), 1)",
        "add_seconds(date('2024-02-29'), 1)",
        "date('2024-02-29') + 1",
        "timestamp('2024-02-29T00:00:00Z') - 1",
    ] {
        rejected(expression);
    }
}

#[test]
fn temporal_builtins_enforce_exact_arity_and_operand_types() {
    for expression in [
        "date()",
        "date('2024-02-29', 'UTC')",
        "date(0)",
        "date(null)",
        "date(true)",
        "date(date('2024-02-29'))",
        "timestamp()",
        "timestamp(0)",
        "timestamp(null)",
        "timestamp('2024-02-29T00:00:00Z', 'UTC')",
        "days_between()",
        "days_between(date('2024-02-29'))",
        "days_between(date('2024-02-29'), date('2024-03-01'), 0)",
        "seconds_between()",
        "seconds_between(timestamp('2024-02-29T00:00:00Z'))",
        "seconds_between(timestamp('2024-02-29T00:00:00Z'), timestamp('2024-03-01T00:00:00Z'), 0)",
        "add_days()",
        "add_days(date('2024-02-29'))",
        "add_days(date('2024-02-29'), 1, 1)",
        "add_days(date('2024-02-29'), 0.5)",
        "add_days(date('2024-02-29'), '1')",
        "add_days(date('2024-02-29'), null)",
        "add_seconds()",
        "add_seconds(timestamp('2024-02-29T00:00:00Z'))",
        "add_seconds(timestamp('2024-02-29T00:00:00Z'), 1, 1)",
        "add_seconds(timestamp('2024-02-29T00:00:00Z'), 0.5)",
        "add_seconds(timestamp('2024-02-29T00:00:00Z'), '1')",
    ] {
        rejected(expression);
    }
}

#[test]
fn legacy_temporal_and_ambient_clock_paths_are_unavailable() {
    for expression in [
        "date()",
        "now()",
        "d()",
        "d('2024-02-29T00:00:00Z')",
        "time('12:34:56')",
        "duration('1d')",
        "year('2024-02-29')",
        "dayOfWeek('2024-02-29')",
        "dayOfMonth('2024-02-29')",
        "dayOfYear('2024-02-29')",
        "weekOfYear('2024-02-29')",
        "monthOfYear('2024-02-29')",
        "monthString('2024-02-29')",
        "dateString('2024-02-29')",
        "weekdayString('2024-02-29')",
        "startOf('2024-02-29', 'day')",
        "endOf('2024-02-29', 'day')",
        "date('2024-02-29').isToday()",
        "date('2024-02-29').isYesterday()",
        "date('2024-02-29').isTomorrow()",
        "date('2024-02-29').tz('Europe/London')",
        "date('2024-02-29').add(1, 'day')",
        "date('2024-02-29').format()",
        "timestamp('2024-02-29T00:00:00Z').isToday()",
        "timestamp('2024-02-29T00:00:00Z').tz('Europe/London')",
    ] {
        rejected(expression);
    }
}

mod metering {
    use super::*;
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;
    use zen_expression::meter::{BudgetExhausted, Meter};
    use zen_expression::vm::VMError;
    use zen_expression::IsolateError;

    fn exhausted(error: IsolateError) -> BudgetExhausted {
        match error {
            IsolateError::VMError {
                source: VMError::BudgetExhausted(error),
            } => error,
            other => panic!("expected temporal budget exhaustion, got {other:?}"),
        }
    }

    #[test]
    fn parsing_and_arithmetic_costs_are_pinned_on_both_targets() {
        for (expression, expected) in [
            ("date('2024-02-29')", 12),
            ("timestamp('2024-02-29T00:00:00Z')", 22),
            ("timestamp('2024-02-29T00:00:00.123456789Z')", 32),
            ("add_days(date('2024-02-29'), 1)", 24),
            ("days_between(date('2024-02-29'), date('2024-03-01'))", 35),
            ("add_seconds(timestamp('2024-02-29T00:00:00Z'), 1)", 34),
            (
                "seconds_between(timestamp('2024-02-29T00:00:00Z'), timestamp('2024-03-01T00:00:00Z'))",
                55,
            ),
        ] {
            let meter = Meter::new(u64::MAX);
            Isolate::new()
                .with_meter(Some(meter.clone()))
                .run_standard(expression)
                .unwrap();
            assert_eq!(meter.used(), expected, "{expression}");
            assert_eq!(meter.exhausted(), None);
        }
    }

    #[test]
    fn parsing_and_arithmetic_exhaust_before_work_and_remain_sticky() {
        for (expression, limit, used) in [
            ("date('2024-02-29')", 11, 12),
            ("timestamp('2024-02-29T00:00:00Z')", 21, 22),
            ("add_days(date('2024-02-29'), 1)", 23, 24),
            ("add_seconds(timestamp('2024-02-29T00:00:00Z'), 1)", 33, 34),
        ] {
            let meter = Meter::new(limit);
            let mut isolate = Isolate::new().with_meter(Some(meter.clone()));
            let expected = BudgetExhausted { limit, used };
            assert_eq!(
                exhausted(isolate.run_standard(expression).unwrap_err()),
                expected,
                "{expression}"
            );
            assert_eq!(meter.exhausted(), Some(expected));
            assert_eq!(
                exhausted(isolate.run_standard("date('2000-02-29')").unwrap_err()),
                expected
            );
        }
        // A large malformed string pays by byte length before validation; this
        // must report fuel exhaustion instead of reaching the temporal parser.
        for expression in ["date(value)", "timestamp(value)"] {
            let meter = Meter::new(100);
            let mut isolate =
                Isolate::with_environment(json!({ "value": "x".repeat(10_000) }).into())
                    .with_meter(Some(meter.clone()));
            assert_eq!(
                exhausted(isolate.run_standard(expression).unwrap_err()),
                BudgetExhausted {
                    limit: 100,
                    used: 10_002
                }
            );
            assert_eq!(meter.scan_steps(), 0);
        }
    }
}
