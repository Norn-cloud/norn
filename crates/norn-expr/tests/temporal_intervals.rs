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

    // Partially overlapping distinct payload intervals rejected (even if both slices are valid TZif).
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
        Err(TzdbBundleError::InvalidTzif {
            zone: "UTC".to_owned()
        })
    );

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
