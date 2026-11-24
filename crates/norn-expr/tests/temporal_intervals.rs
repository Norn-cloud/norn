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

//! `norn-zen/7` temporal interval and DST resolution conformance tests (#1584),
//! and the Allen-relation and civil-day laws of `verification/owl-time-v1` (#1713).

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

/// Every proper interval `[start, end)` with endpoints in `0..6`.
fn grid_intervals() -> Vec<(u8, u8)> {
    (0..6_u8)
        .flat_map(|start| (start + 1..6).map(move |end| (start, end)))
        .collect()
}

/// The grid instant `second` as a timestamp literal.
fn grid_timestamp(second: u8) -> String {
    format!("timestamp('2025-03-09T14:00:0{second}Z')")
}

/// Exhaustive-finite: all 225 ordered pairs of the 15 proper intervals with
/// endpoints in `0..6`, and all 3,375 triples for the compositions (#1713).
#[test]
fn allen_relations_partition_the_grid_and_agree_with_the_interval_builtins() {
    use norn_expr::temporal::{AllenRelation, allen_relation};

    let intervals = grid_intervals();
    assert_eq!(intervals.len(), 15);
    let mut seen = Vec::new();
    for &(a, b) in &intervals {
        for &(c, d) in &intervals {
            let pair = format!("[{a}, {b}) and [{c}, {d})");

            // Jointly exhaustive and pairwise disjoint: exactly one endpoint
            // predicate holds, and the classifier returns it.
            let relation = allen_relation(&a, &b, &c, &d).expect("proper intervals");
            let holding: Vec<AllenRelation> = AllenRelation::ALL
                .into_iter()
                .filter(|candidate| candidate.holds(&a, &b, &c, &d))
                .collect();
            assert_eq!(holding, vec![relation], "{pair}");

            // Converse: swapping the intervals swaps every relation.
            assert_eq!(
                allen_relation(&c, &d, &a, &b),
                Some(relation.converse()),
                "{pair}"
            );
            for candidate in AllenRelation::ALL {
                assert_eq!(candidate.converse().converse(), candidate);
                assert_eq!(
                    candidate.holds(&a, &b, &c, &d),
                    candidate.converse().holds(&c, &d, &a, &b),
                    "{candidate:?} for {pair}"
                );
            }

            // Boundary: the relation shares an instant exactly when some grid
            // instant lies in both half-open intervals.
            let shared = (0..6_u8).any(|t| a <= t && t < b && c <= t && t < d);
            assert_eq!(relation.shares_instant(), shared, "{pair}");
            assert_eq!(
                shared,
                !matches!(
                    relation,
                    AllenRelation::Before
                        | AllenRelation::After
                        | AllenRelation::Meets
                        | AllenRelation::MetBy
                ),
                "{pair}"
            );

            // The shipped `norn-zen/7` builtins agree on the same intervals.
            let args = [a, b, c, d].map(grid_timestamp).join(", ");
            let builtin = |name: &str| {
                eval(&format!("{name}({args})"), Value::object::<&str>([]))
                    .unwrap_or_else(|error| panic!("{name} for {pair}: {error:?}"))
            };
            assert_eq!(builtin("intervals_overlap"), Value::Bool(shared), "{pair}");
            let gap = i16::from(c) - i16::from(b);
            let gap = gap.max(i16::from(a) - i16::from(d)).max(0);
            assert_eq!(
                builtin("interval_gap_seconds"),
                num(&gap.to_string()),
                "{pair}"
            );
            assert_eq!(
                gap > 0,
                matches!(relation, AllenRelation::Before | AllenRelation::After),
                "{pair}"
            );
            let intersection = (i16::from(b.min(d)) - i16::from(a.max(c))).max(0);
            assert_eq!(
                builtin("interval_intersection_seconds"),
                num(&intersection.to_string()),
                "{pair}"
            );
            assert_eq!(intersection > 0, shared, "{pair}");

            if !seen.contains(&relation) {
                seen.push(relation);
            }
        }
    }
    // Non-vacuity: the grid realizes every one of the thirteen relations.
    assert_eq!(seen.len(), AllenRelation::ALL.len());

    // Adjacent `[a, b)` and `[b, c)` meet without overlapping.
    assert_eq!(allen_relation(&0, &2, &2, &5), Some(AllenRelation::Meets));
    assert!(!AllenRelation::Meets.shares_instant());

    // Compositions used for scheduling.
    let relate = |x: (u8, u8), y: (u8, u8)| allen_relation(&x.0, &x.1, &y.0, &y.1);
    let mut composed = 0_u32;
    for &x in &intervals {
        for &y in &intervals {
            for &z in &intervals {
                let expected = match (relate(x, y), relate(y, z)) {
                    (Some(AllenRelation::Before), Some(AllenRelation::Before))
                    | (Some(AllenRelation::Meets), Some(AllenRelation::Meets))
                    | (Some(AllenRelation::Before), Some(AllenRelation::Meets))
                    | (Some(AllenRelation::Meets), Some(AllenRelation::Before))
                    | (Some(AllenRelation::During), Some(AllenRelation::Before)) => {
                        AllenRelation::Before
                    }
                    (Some(AllenRelation::During), Some(AllenRelation::During)) => {
                        AllenRelation::During
                    }
                    (Some(AllenRelation::Contains), Some(AllenRelation::Contains)) => {
                        AllenRelation::Contains
                    }
                    (Some(AllenRelation::After), Some(AllenRelation::After)) => {
                        AllenRelation::After
                    }
                    _ => continue,
                };
                assert_eq!(relate(x, z), Some(expected), "{x:?} {y:?} {z:?}");
                composed += 1;
            }
        }
    }
    assert!(composed > 0);

    // Fail closed: an improper interval has no relation and satisfies none.
    for (start, end, other_start, other_end) in [(3, 3, 0, 1), (4, 2, 0, 1), (0, 1, 5, 5)] {
        assert_eq!(allen_relation(&start, &end, &other_start, &other_end), None);
        assert!(
            AllenRelation::ALL
                .into_iter()
                .all(|relation| !relation.holds(&start, &end, &other_start, &other_end))
        );
    }
}

/// A civil day is one calendar date; its physical length is whatever the
/// pinned zone rules make it. Real 23-hour, 24-hour and 25-hour days (and
/// Lord Howe's 23.5-hour and 24.5-hour days) from both pinned tzdb bundles,
/// probed at every wall-clock minute (#1713).
#[test]
fn civil_days_keep_their_physical_length_and_dst_gaps_fail_closed() {
    use norn_expr::temporal::{
        DstPolicy, TemporalError, TzdbBundle, resolve_local_timestamp_in, timestamp_ordering_key,
    };

    const POLICIES: [DstPolicy; 3] = [DstPolicy::Reject, DstPolicy::Earlier, DstPolicy::Later];
    // (zone, civil day, next civil day, gap minutes, fold minutes, physical seconds)
    const DAYS: [(&str, &str, &str, i64, i64, i64); 8] = [
        (
            "America/New_York",
            "2025-03-09",
            "2025-03-10",
            60,
            0,
            82_800,
        ),
        ("America/New_York", "2025-06-15", "2025-06-16", 0, 0, 86_400),
        (
            "America/New_York",
            "2025-11-02",
            "2025-11-03",
            0,
            60,
            90_000,
        ),
        // Egypt moves its clocks at local midnight.
        ("Africa/Cairo", "2025-04-25", "2025-04-26", 60, 0, 82_800),
        ("Africa/Cairo", "2025-07-01", "2025-07-02", 0, 0, 86_400),
        ("Africa/Cairo", "2025-10-30", "2025-10-31", 0, 60, 90_000),
        // Lord Howe Island shifts by half an hour.
        (
            "Australia/Lord_Howe",
            "2025-10-05",
            "2025-10-06",
            30,
            0,
            84_600,
        ),
        (
            "Australia/Lord_Howe",
            "2025-04-06",
            "2025-04-07",
            0,
            30,
            88_200,
        ),
    ];

    let seconds = |utc: &str| timestamp_ordering_key(utc).expect("canonical UTC").0;
    for bundle in [TzdbBundle::v2025b(), TzdbBundle::v2026c()] {
        for (zone, day, next, gap, fold, physical) in DAYS {
            let context = format!("{zone} {day} (tzdb {})", bundle.release());
            let resolve =
                |local: &str, policy| resolve_local_timestamp_in(local, zone, policy, bundle);
            // The first instant of a civil day: its first wall-clock minute
            // that exists (01:00 when the clocks spring forward at midnight).
            let first_instant = |date: &str| {
                (0..1_440)
                    .find_map(|minute| {
                        let local = format!("{date}T{:02}:{:02}:00", minute / 60, minute % 60);
                        resolve(&local, DstPolicy::Earlier).ok()
                    })
                    .map(|utc| seconds(&utc))
                    .expect("a civil day has an instant")
            };

            let (mut gap_minutes, mut fold_minutes) = (0_i64, 0_i64);
            for minute in 0..1_440 {
                let local = format!("{day}T{:02}:{:02}:00", minute / 60, minute % 60);
                let [reject, earlier, later] = POLICIES.map(|policy| resolve(&local, policy));
                match reject {
                    // An unambiguous wall time needs no policy.
                    Ok(_) => {
                        assert_eq!(earlier, reject, "{context} {local}");
                        assert_eq!(later, reject, "{context} {local}");
                    }
                    // A nonexistent wall time is refused under every policy.
                    Err(TemporalError::NonexistentLocalTime) => {
                        gap_minutes += 1;
                        assert_eq!(earlier, reject, "{context} {local}");
                        assert_eq!(later, reject, "{context} {local}");
                    }
                    // A repeated wall time is refused unless a side is chosen,
                    // and the two sides are the fold apart.
                    Err(TemporalError::AmbiguousLocalTime) => {
                        fold_minutes += 1;
                        let earlier = seconds(&earlier.expect("earlier side"));
                        let later = seconds(&later.expect("later side"));
                        assert_eq!(later - earlier, fold * 60, "{context} {local}");
                    }
                    Err(other) => panic!("{context} {local}: {other:?}"),
                }
            }
            assert_eq!((gap_minutes, fold_minutes), (gap, fold), "{context}");

            // One civil day, and never a silently assumed 24 hours.
            let length = first_instant(next) - first_instant(day);
            assert_eq!(length, physical, "{context}");
            assert_eq!(length, 86_400 - gap * 60 + fold * 60, "{context}");
        }

        // Cairo, 2025-04-25: the civil day has no midnight. The last second of
        // the 24th and the first instant of the 25th (01:00) are one second
        // apart, and nothing in between resolves under any policy.
        let cairo =
            |local: &str, policy| resolve_local_timestamp_in(local, "Africa/Cairo", policy, bundle);
        for policy in POLICIES {
            for missing in [
                "2025-04-25T00:00:00",
                "2025-04-25T00:30:00",
                "2025-04-25T00:59:59.999999999",
            ] {
                assert_eq!(
                    cairo(missing, policy),
                    Err(TemporalError::NonexistentLocalTime),
                    "{missing} under {policy:?}"
                );
            }
            assert_eq!(
                cairo("2025-04-24T23:59:59", policy).as_deref(),
                Ok("2025-04-24T21:59:59Z")
            );
            assert_eq!(
                cairo("2025-04-25T01:00:00", policy).as_deref(),
                Ok("2025-04-24T22:00:00Z")
            );
        }
    }

    // The same separation in checked expressions: one civil day apart is
    // 23, 24 or 25 physical hours, and a physical day added to a 23-hour
    // day's midnight lands at 01:00 on the next civil day.
    let empty = || Value::object::<&str>([]);
    let physical = |zone: &str, day: &str, next: &str| {
        format!(
            "seconds_between(resolve_local_timestamp('{day}T00:00:00', '{zone}', 'reject'), \
             resolve_local_timestamp('{next}T00:00:00', '{zone}', 'reject'))"
        )
    };
    for (zone, day, next, expected) in [
        ("America/New_York", "2025-03-09", "2025-03-10", "82800"),
        ("America/New_York", "2025-06-15", "2025-06-16", "86400"),
        ("America/New_York", "2025-11-02", "2025-11-03", "90000"),
        ("Africa/Cairo", "2025-10-30", "2025-10-31", "90000"),
    ] {
        assert_eq!(
            eval(
                &format!("days_between(date('{day}'), date('{next}'))"),
                empty()
            )
            .unwrap(),
            num("1")
        );
        assert_eq!(
            eval(&physical(zone, day, next), empty()).unwrap(),
            num(expected),
            "{zone} {day}"
        );
    }
    assert_eq!(
        eval(
            "add_seconds(resolve_local_timestamp('2025-03-09T00:00:00', 'America/New_York', 'reject'), 86400) \
             == resolve_local_timestamp('2025-03-10T01:00:00', 'America/New_York', 'reject')",
            empty()
        )
        .unwrap(),
        Value::Bool(true)
    );
    // Cairo's 2025-04-25 has no midnight, so measuring it from midnight is a
    // refusal, not a 24-hour answer.
    assert!(matches!(
        eval(
            &physical("Africa/Cairo", "2025-04-25", "2025-04-26"),
            empty()
        ),
        Err(ExprError::Evaluation { .. })
    ));
}

#[test]
fn versioned_tzdb_2025b_and_2026c_edmonton_regression_and_isolation()
-> Result<(), Box<dyn std::error::Error>> {
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
    let expr = Expression::compile(source)?;
    let ts_utc = Type::Timestamp {
        timezone: TimezonePolicy::Utc,
    };
    let checked_25 = check_expression_with_in(&expr, &env, &ts_utc, &limits, b25)
        .map_err(|diags| format!("check 2025b: {diags:?}"))?;
    let checked_26 = check_expression_with_in(&expr, &env, &ts_utc, &limits, b26)
        .map_err(|diags| format!("check 2026c: {diags:?}"))?;
    let root = Value::object([("input", Value::object(std::iter::empty::<(&str, Value)>()))]);

    // Interleaved same-thread evaluations prove zero thread-local/global leakage.
    for _ in 0..4 {
        let out_25 =
            expr.evaluate_in(&root, EvaluationBudget::new(checked_25.cost().units()), b25)?;
        let out_26 =
            expr.evaluate_in(&root, EvaluationBudget::new(checked_26.cost().units()), b26)?;
        assert_eq!(out_25.value(), &Value::from("2027-01-01T06:30:00Z"));
        assert_eq!(out_26.value(), &Value::from("2027-01-01T05:30:00Z"));
    }

    // PreparedAdmission is digest-bound to the TzdbBundle it was admitted under.
    let prepared_25 = expr.prepare_admitted_in(&env, &ts_utc, b25);
    let prepared_26 = expr.prepare_admitted_in(&env, &ts_utc, b26);
    assert_eq!(prepared_25.tzdb_sha256(), b25.sha256());
    assert_eq!(prepared_26.tzdb_sha256(), b26.sha256());
    assert_eq!(
        expr.evaluate_prepared_in(
            &prepared_25,
            &root,
            EvaluationBudget::default(),
            &env,
            &ts_utc,
            b25
        )
        .result?
        .value(),
        &Value::from("2027-01-01T06:30:00Z")
    );
    assert_eq!(
        expr.evaluate_prepared_in(
            &prepared_26,
            &root,
            EvaluationBudget::default(),
            &env,
            &ts_utc,
            b26
        )
        .result?
        .value(),
        &Value::from("2027-01-01T05:30:00Z")
    );
    assert!(matches!(
        expr.evaluate_prepared_in(
            &prepared_25,
            &root,
            EvaluationBudget::default(),
            &env,
            &ts_utc,
            b26
        )
        .result,
        Err(ExprError::TzdbMismatch { .. })
    ));
    assert!(matches!(
        expr.evaluate_prepared(
            &prepared_26,
            &root,
            EvaluationBudget::default(),
            &env,
            &ts_utc
        )
        .result,
        Err(ExprError::TzdbMismatch { .. })
    ));

    // UnaryTest::evaluate_in honors explicit bundle selection too.
    let unary = UnaryTest::compile(
        "timestamp($) == resolve_local_timestamp('2026-12-31T23:30:00', 'America/Edmonton', 'reject')",
    )?;
    let subject = Value::from("2027-01-01T06:30:00Z");
    assert!(unary.evaluate_in(&root, &subject, EvaluationBudget::default(), b25)?);
    assert!(!unary.evaluate_in(&root, &subject, EvaluationBudget::default(), b26)?);

    // Concurrent multi-thread evaluations with 2025b and 2026c produce exact independent results.
    let handles: Vec<_> = (0..8)
        .map(|worker| {
            let expr = expr.clone();
            let root = root.clone();
            std::thread::spawn(move || -> Result<(), ExprError> {
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
                    let result = expr.evaluate_in(&root, EvaluationBudget::default(), bundle)?;
                    assert_eq!(result.value(), &expected);
                }
                Ok(())
            })
        })
        .collect();
    for handle in handles {
        handle.join().map_err(|_panic| "worker thread panicked")??;
    }

    // Literal admission honors the explicit bundle (custom subset bundle with only UTC rejects America/Edmonton).
    let utc_tzif = b25.tzif("UTC").ok_or("missing UTC tzif")?;
    let utc_len = u32::try_from(utc_tzif.len())?;
    let mut only_utc = Vec::new();
    only_utc.extend_from_slice(norn_expr::temporal::TZDB_BUNDLE_MAGIC);
    only_utc.extend_from_slice(&5_u16.to_le_bytes()); // "2099u"
    only_utc.extend_from_slice(&0_u16.to_le_bytes());
    only_utc.extend_from_slice(&1_u32.to_le_bytes());
    only_utc.extend_from_slice(&3_u32.to_le_bytes());
    only_utc.extend_from_slice(&utc_len.to_le_bytes());
    only_utc.extend_from_slice(b"2099u");
    only_utc.extend_from_slice(&0_u32.to_le_bytes());
    only_utc.extend_from_slice(&3_u32.to_le_bytes());
    only_utc.extend_from_slice(&0_u32.to_le_bytes());
    only_utc.extend_from_slice(&utc_len.to_le_bytes());
    only_utc.extend_from_slice(b"UTC");
    only_utc.extend_from_slice(utc_tzif);
    let utc_bundle = TzdbBundle::from_bytes(&only_utc)?;

    assert!(
        check_expression_with_in(&expr, &env, &ts_utc, &limits, &utc_bundle).is_err(),
        "America/Edmonton must be rejected by literal admission under a UTC-only bundle"
    );
    let utc_expr =
        Expression::compile("resolve_local_timestamp('2026-12-31T23:30:00', 'UTC', 'reject')")?;
    assert!(check_expression_with_in(&utc_expr, &env, &ts_utc, &limits, &utc_bundle).is_ok());
    Ok(())
}

fn encode_tzdb_raw(
    release: &str,
    entries: &[(&str, usize, usize)],
    payload: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let release_bytes = release.as_bytes();
    let mut names = Vec::new();
    let mut index = Vec::new();
    for &(name, tzif_off, tzif_len) in entries {
        let name_off = u32::try_from(names.len())?;
        let name_len = u32::try_from(name.len())?;
        names.extend_from_slice(name.as_bytes());
        index.extend_from_slice(&name_off.to_le_bytes());
        index.extend_from_slice(&name_len.to_le_bytes());
        index.extend_from_slice(&u32::try_from(tzif_off)?.to_le_bytes());
        index.extend_from_slice(&u32::try_from(tzif_len)?.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(norn_expr::temporal::TZDB_BUNDLE_MAGIC);
    out.extend_from_slice(&u16::try_from(release_bytes.len())?.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&u32::try_from(entries.len())?.to_le_bytes());
    out.extend_from_slice(&u32::try_from(names.len())?.to_le_bytes());
    out.extend_from_slice(&u32::try_from(payload.len())?.to_le_bytes());
    out.extend_from_slice(release_bytes);
    out.extend_from_slice(&index);
    out.extend_from_slice(&names);
    out.extend_from_slice(payload);
    Ok(out)
}

fn encode_tzdb_synthetic(
    release: &str,
    entries: &[(&str, &[u8])],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut payload = Vec::new();
    let mut indexed = Vec::with_capacity(entries.len());
    for &(name, tzif) in entries {
        let off = payload.len();
        payload.extend_from_slice(tzif);
        indexed.push((name, off, tzif.len()));
    }
    encode_tzdb_raw(release, &indexed, &payload)
}

#[test]
fn tzdb_bundles_decode_and_catalog_enforces_immutable_releases()
-> Result<(), Box<dyn std::error::Error>> {
    use norn_expr::temporal::{
        TZDB_RELEASE_2025B, TZDB_RELEASE_2026C, TZDB_SHA256_2025B, TZDB_SHA256_2026C, TzdbBundle,
        TzdbBundleError, TzdbCatalog,
    };

    let b25 = TzdbBundle::v2025b();
    let b26 = TzdbBundle::v2026c();
    assert_eq!(b25.release(), TZDB_RELEASE_2025B);
    assert_eq!(b26.release(), TZDB_RELEASE_2026C);
    assert_eq!(b25.sha256(), TZDB_SHA256_2025B);
    assert_eq!(b26.sha256(), TZDB_SHA256_2026C);
    assert_eq!(b25.zone_count(), 597);
    assert_eq!(b26.zone_count(), 597);
    assert!(!b25.contains_zone("Factory"));
    assert!(!b26.contains_zone("Factory"));

    let decoded_25 = TzdbBundle::from_bytes_verified(
        include_bytes!("../data/tzdb/2025b.tzdb"),
        TZDB_SHA256_2025B,
    )?;
    let decoded_26 = TzdbBundle::from_bytes_verified(
        include_bytes!("../data/tzdb/2026c.tzdb"),
        TZDB_SHA256_2026C,
    )?;
    assert_eq!(&decoded_25, b25);
    assert_eq!(&decoded_26, b26);

    let catalog = TzdbCatalog::builtin();
    assert_eq!(
        catalog.releases().collect::<Vec<_>>(),
        vec!["2025b", "2026c"]
    );
    assert_eq!(catalog.get_verified("2026c", TZDB_SHA256_2026C), Some(b26));
    assert_eq!(catalog.get_verified("2026c", TZDB_SHA256_2025B), None);
    assert_eq!(catalog.get_by_sha256(TZDB_SHA256_2025B), Some(b25));
    assert_eq!(catalog.default_bundle(), b25);

    // Idempotent re-registration of identical (release, sha256) succeeds.
    let same = catalog.clone().with_bundle(b25.clone())?;
    assert_eq!(same.default_bundle(), b25);

    // Conflicting bundle claiming an existing release (e.g. "2025b") is refused.
    let utc_tzif = b25.tzif("UTC").ok_or("missing UTC tzif")?;
    let impostor = TzdbBundle::from_bytes(&encode_tzdb_synthetic("2025b", &[("UTC", utc_tzif)])?)?;
    assert_eq!(
        catalog.clone().with_bundle(impostor),
        Err(TzdbBundleError::DuplicateRelease {
            release: "2025b".to_owned()
        })
    );

    // Distinct release id installs cleanly without changing default.
    let custom = TzdbBundle::from_bytes(&encode_tzdb_synthetic("2099z", &[("UTC", utc_tzif)])?)?;
    let extended = catalog.with_bundle(custom.clone())?;
    assert_eq!(extended.get("2099z"), Some(&custom));
    assert_eq!(extended.default_bundle(), b25);
    Ok(())
}

#[test]
fn tzdb_decoder_rejects_malformed_overlapping_trailing_and_oversized_bundles()
-> Result<(), Box<dyn std::error::Error>> {
    use norn_expr::temporal::{MAX_TZDB_BUNDLE_BYTES, TzdbBundle, TzdbBundleError};

    let utc_tzif = TzdbBundle::v2025b().tzif("UTC").ok_or("missing UTC tzif")?;

    // Valid shared-alias bundle passes.
    let valid = encode_tzdb_raw(
        "2099z",
        &[("Etc/UTC", 0, utc_tzif.len()), ("UTC", 0, utc_tzif.len())],
        utc_tzif,
    )?;
    assert_eq!(TzdbBundle::from_bytes(&valid)?.zone_count(), 2);

    // Digest mismatch, bad magic, truncated header, non-zero reserved, length mismatch.
    assert!(matches!(
        TzdbBundle::from_bytes_verified(&valid, &"0".repeat(64)),
        Err(TzdbBundleError::DigestMismatch { .. })
    ));
    let mut bad_magic = valid.clone();
    *bad_magic.get_mut(0).ok_or("byte 0")? = b'X';
    assert_eq!(
        TzdbBundle::from_bytes(&bad_magic),
        Err(TzdbBundleError::InvalidMagic)
    );
    assert_eq!(
        TzdbBundle::from_bytes(b"NTZDB01"),
        Err(TzdbBundleError::TruncatedHeader)
    );
    let mut bad_reserved = valid.clone();
    *bad_reserved.get_mut(10).ok_or("byte 10")? = 1;
    assert_eq!(
        TzdbBundle::from_bytes(&bad_reserved),
        Err(TzdbBundleError::NonZeroReserved)
    );
    let mut trailing = valid;
    trailing.push(0);
    assert!(matches!(
        TzdbBundle::from_bytes(&trailing),
        Err(TzdbBundleError::LengthMismatch { .. })
    ));

    // Unsorted, duplicate, Factory, and invalid TZif rejected.
    let unsorted = encode_tzdb_synthetic("2099z", &[("UTC", utc_tzif), ("Etc/UTC", utc_tzif)])?;
    assert_eq!(
        TzdbBundle::from_bytes(&unsorted),
        Err(TzdbBundleError::UnsortedOrDuplicateZone { index: 1 })
    );
    let duplicate = encode_tzdb_synthetic("2099z", &[("UTC", utc_tzif), ("UTC", utc_tzif)])?;
    assert_eq!(
        TzdbBundle::from_bytes(&duplicate),
        Err(TzdbBundleError::UnsortedOrDuplicateZone { index: 1 })
    );
    let factory = encode_tzdb_synthetic("2099z", &[("Factory", utc_tzif)])?;
    assert_eq!(
        TzdbBundle::from_bytes(&factory),
        Err(TzdbBundleError::ForbiddenZone {
            name: "Factory".to_owned()
        })
    );
    let bad_tzif = encode_tzdb_synthetic("2099z", &[("UTC", b"not-a-valid-tzif-payload")])?;
    assert_eq!(
        TzdbBundle::from_bytes(&bad_tzif),
        Err(TzdbBundleError::InvalidTzif {
            zone: "UTC".to_owned()
        })
    );

    // Trailing garbage byte or newline after valid TZif footer is rejected by exact RFC 8536 framing.
    let mut tzif_plus_junk = utc_tzif.to_vec();
    tzif_plus_junk.push(b'X');
    let junk_bundle = encode_tzdb_synthetic("2099z", &[("UTC", &tzif_plus_junk)])?;
    assert_eq!(
        TzdbBundle::from_bytes(&junk_bundle),
        Err(TzdbBundleError::InvalidTzif {
            zone: "UTC".to_owned()
        })
    );
    let mut tzif_plus_nl = utc_tzif.to_vec();
    tzif_plus_nl.push(b'\n');
    let nl_bundle = encode_tzdb_synthetic("2099z", &[("UTC", &tzif_plus_nl)])?;
    assert_eq!(
        TzdbBundle::from_bytes(&nl_bundle),
        Err(TzdbBundleError::InvalidTzif {
            zone: "UTC".to_owned()
        })
    );

    // Partially overlapping distinct payload intervals fail deterministically with the partition error
    // BEFORE any per-blob TZif parsing runs.
    let mut two_blobs = Vec::new();
    two_blobs.extend_from_slice(utc_tzif);
    two_blobs.extend_from_slice(utc_tzif);
    let partial_overlap = encode_tzdb_raw(
        "2099z",
        &[("Etc/UTC", 0, utc_tzif.len()), ("UTC", 0, two_blobs.len())],
        &two_blobs,
    )?;
    assert_eq!(
        TzdbBundle::from_bytes(&partial_overlap),
        Err(TzdbBundleError::OverlappingOrUnreferencedPayload)
    );

    // RFC 9636 §3.3.2: TZif V2 rejects V3-only transition rule hours (< 0 or > 24),
    // while TZif V3 accepts the exact same footer.
    let make_tzif_with_footer = |version: u8, footer: &str| -> Vec<u8> {
        let mut blob = Vec::new();
        // V1 header (44 bytes): typecnt = 1, charcnt = 4 ("UTC\0"), all other counts = 0.
        blob.extend_from_slice(b"TZif");
        blob.push(version);
        blob.extend_from_slice(&[0_u8; 15]);
        blob.extend_from_slice(&0_u32.to_be_bytes()); // isutcnt
        blob.extend_from_slice(&0_u32.to_be_bytes()); // isstdcnt
        blob.extend_from_slice(&0_u32.to_be_bytes()); // leapcnt
        blob.extend_from_slice(&0_u32.to_be_bytes()); // timecnt
        blob.extend_from_slice(&1_u32.to_be_bytes()); // typecnt
        blob.extend_from_slice(&4_u32.to_be_bytes()); // charcnt
        // V1 data: 1 ttinfo (6 bytes) + "UTC\0" (4 bytes).
        blob.extend_from_slice(&[0_u8; 6]);
        blob.extend_from_slice(b"UTC\0");
        // V2/V3 header + identical data.
        blob.extend_from_slice(b"TZif");
        blob.push(version);
        blob.extend_from_slice(&[0_u8; 15]);
        blob.extend_from_slice(&0_u32.to_be_bytes());
        blob.extend_from_slice(&0_u32.to_be_bytes());
        blob.extend_from_slice(&0_u32.to_be_bytes());
        blob.extend_from_slice(&0_u32.to_be_bytes());
        blob.extend_from_slice(&1_u32.to_be_bytes());
        blob.extend_from_slice(&4_u32.to_be_bytes());
        blob.extend_from_slice(&[0_u8; 6]);
        blob.extend_from_slice(b"UTC\0");
        // Footer: \n<POSIX TZ>\n
        blob.push(b'\n');
        blob.extend_from_slice(footer.as_bytes());
        blob.push(b'\n');
        blob
    };
    for v3_only_footer in [
        "EST5EDT,M3.2.0/26,M11.1.0/2",
        "EST5EDT,M3.2.0/-1,M11.1.0/2",
        "EST5EDT,M3.2.0/+2,M11.1.0/2",
    ] {
        let v2_blob = make_tzif_with_footer(b'2', v3_only_footer);
        let v2_bundle = encode_tzdb_synthetic("2099z", &[("Test/Zone", &v2_blob)])?;
        assert_eq!(
            TzdbBundle::from_bytes(&v2_bundle),
            Err(TzdbBundleError::InvalidTzif {
                zone: "Test/Zone".to_owned()
            }),
            "V2 must reject V3-only footer `{v3_only_footer}`"
        );
        let v3_blob = make_tzif_with_footer(b'3', v3_only_footer);
        let v3_bundle = encode_tzdb_synthetic("2099z", &[("Test/Zone", &v3_blob)])?;
        assert!(
            TzdbBundle::from_bytes(&v3_bundle).is_ok(),
            "V3 must accept footer `{v3_only_footer}`"
        );
    }

    // Unreferenced trailing byte in payload section rejected.
    let mut with_gap = utc_tzif.to_vec();
    with_gap.push(0);
    let unreferenced = encode_tzdb_raw("2099z", &[("UTC", 0, utc_tzif.len())], &with_gap)?;
    assert_eq!(
        TzdbBundle::from_bytes(&unreferenced),
        Err(TzdbBundleError::OverlappingOrUnreferencedPayload)
    );

    // Out-of-bounds TZif offset/len rejected.
    let mut oob = encode_tzdb_synthetic("2099z", &[("UTC", utc_tzif)])?;
    let bad_len = u32::try_from(utc_tzif.len())?
        .checked_add(1)
        .ok_or("u32 overflow")?
        .to_le_bytes();
    oob.get_mut(41..45)
        .ok_or("missing tzif_len slot")?
        .copy_from_slice(&bad_len);
    assert_eq!(
        TzdbBundle::from_bytes(&oob),
        Err(TzdbBundleError::OutOfBoundsEntry { index: 0 })
    );

    // Oversized buffer rejected BEFORE hashing by both from_bytes and from_bytes_verified.
    let oversized = vec![0_u8; MAX_TZDB_BUNDLE_BYTES.checked_add(1).ok_or("size + 1")?];
    assert_eq!(
        TzdbBundle::from_bytes(&oversized),
        Err(TzdbBundleError::TooLarge {
            len: oversized.len(),
            max: MAX_TZDB_BUNDLE_BYTES,
        })
    );
    assert_eq!(
        TzdbBundle::from_bytes_verified(&oversized, &"0".repeat(64)),
        Err(TzdbBundleError::TooLarge {
            len: oversized.len(),
            max: MAX_TZDB_BUNDLE_BYTES,
        })
    );
    Ok(())
}
