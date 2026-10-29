//! Norn `metering`: the operation budget is deterministic.
//!
//! These tests pin exact unit counts. CI runs them natively and on
//! `wasm32-unknown-unknown` (wasm-bindgen-test); both must hit the same numbers.

use serde_json::json;
use zen_expression::meter::{BudgetExhausted, Meter};
use zen_expression::vm::VMError;
use zen_expression::{Isolate, IsolateError};

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

/// Nested closures: 20 outer iterations, each mapping 20 inner elements.
const NESTED: &str = "flatMap([0..19], map([0..19], # * 2))";
/// Units a full evaluation of `NESTED` costs.
const NESTED_FULL: u64 = 3469;
/// A budget smaller than `NESTED_FULL`, and the count at which it runs out.
const SMALL_BUDGET: u64 = 500;
const SMALL_EXHAUSTED_AT: u64 = 502;

fn isolate(meter: &Meter) -> Isolate {
    Isolate::with_environment(json!({ "s": "The quick brown fox" }).into())
        .with_meter(Some(meter.clone()))
}

fn exhausted(err: IsolateError) -> BudgetExhausted {
    match err {
        IsolateError::VMError {
            source: VMError::BudgetExhausted(exhausted),
        } => exhausted,
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
}

#[test]
fn nested_map_flat_map_full_cost_is_pinned() {
    let meter = Meter::new(u64::MAX);
    let result = isolate(&meter).run_standard(NESTED).unwrap();
    assert_eq!(result.as_array().unwrap().borrow().len(), 400);
    assert_eq!(meter.used(), NESTED_FULL);
    assert_eq!(meter.exhausted(), None);
}

#[test]
fn nested_map_flat_map_exhausts_small_budget_at_pinned_count() {
    let meter = Meter::new(SMALL_BUDGET);
    let err = isolate(&meter).run_standard(NESTED).unwrap_err();
    let expected = BudgetExhausted {
        limit: SMALL_BUDGET,
        used: SMALL_EXHAUSTED_AT,
    };
    assert_eq!(exhausted(err), expected);
    assert_eq!(meter.exhausted(), Some(expected));

    // Sticky: later work on the same meter fails with the first exhaustion.
    let err = isolate(&meter).run_standard("1 + 1").unwrap_err();
    assert_eq!(exhausted(err), expected);
}

#[test]
fn string_and_regex_builtins_are_charged_by_size() {
    let short = Meter::new(u64::MAX);
    isolate(&short)
        .run_standard(r#"matches("ab", "a+")"#)
        .unwrap();
    let long = Meter::new(u64::MAX);
    isolate(&long)
        .run_standard(r#"matches("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab", "a+")"#)
        .unwrap();
    // One opcode sequence, so the difference is exactly the extra input bytes.
    assert_eq!(long.used() - short.used(), 39);
    // Regex compilation carries a fixed surcharge over a plain string builtin.
    let plain = Meter::new(u64::MAX);
    isolate(&plain)
        .run_standard(r#"contains("ab", "a+")"#)
        .unwrap();
    assert_eq!(short.used() - plain.used(), 64);
}

#[test]
fn bounded_string_builtins_charge_scans_and_result_copying() {
    let short = cost_with("tokens(s, 'alnum', 256, 256)", json!({ "s": "a" }));
    let long = cost_with(
        "tokens(s, 'alnum', 256, 256)",
        json!({ "s": "a".repeat(40) }),
    );
    assert_eq!(long - short, 78);

    let short = cost_with("take(s, 1)", json!({ "s": "a" }));
    let long = cost_with("take(s, 1)", json!({ "s": "ab" }));
    // Input bytes plus the maximum copied-prefix byte allowance.
    assert_eq!(long - short, 2);

    let short = cost_with("join(parts, '-')", json!({ "parts": ["a", "b"] }));
    let long = cost_with("join(parts, '-')", json!({ "parts": ["a", "BBBB"] }));
    assert_eq!(long - short, 6);
}

#[test]
fn bounded_join_measures_input_before_scanning_it() {
    let parts: Vec<String> = (0..1_000).map(|_| "x".to_owned()).collect();
    let meter = Meter::new(20);
    let err = Isolate::with_environment(json!({ "parts": parts }).into())
        .with_meter(Some(meter.clone()))
        .run_standard("join(parts, '-')")
        .unwrap_err();
    assert!(exhausted(err).used > 20);
    assert!(meter.scan_steps() < 1_000);
}

#[test]
fn bounded_join_gates_large_empty_arrays_before_scanning() {
    let parts = vec![String::new(); 1_000_000];
    let meter = Meter::new(20);
    let err = Isolate::with_environment(json!({ "parts": parts }).into())
        .with_meter(Some(meter.clone()))
        .run_standard("join(parts, '')")
        .unwrap_err();
    let failure = exhausted(err);
    assert_eq!(failure.limit, 20);
    assert!(failure.used > failure.limit);
    assert_eq!(meter.exhausted(), Some(failure));
    assert!(meter.scan_steps() <= 20);
}

#[test]
fn large_interval_is_charged_before_it_is_materialized() {
    let meter = Meter::new(1_000);
    let err = isolate(&meter)
        .run_standard("count([0..1000000000], # > 0)")
        .unwrap_err();
    assert!(exhausted(err).used > 1_000_000_000);
}

fn cost_with(expression: &str, env: serde_json::Value) -> u64 {
    let meter = Meter::new(u64::MAX);
    Isolate::with_environment(env.into())
        .with_meter(Some(meter.clone()))
        .run_standard(expression)
        .unwrap();
    meter.used()
}

/// `flatten` and `merge` copy every child of their argument; they must pay for them,
/// not just for the (length-1) outer array.
#[test]
fn nested_builtins_are_charged_by_the_data_they_traverse() {
    let big: Vec<u64> = (0..1000).collect();
    let small = cost_with("flatten(x)", json!({ "x": [[1]] }));
    let large = cost_with("flatten(x)", json!({ "x": [big] }));
    assert_eq!(large - small, 999);

    let wide: serde_json::Map<String, serde_json::Value> =
        (0..1000).map(|i| (format!("k{i}"), json!(i))).collect();
    let small = cost_with("merge(x)", json!({ "x": [{ "k": 1 }] }));
    let large = cost_with("merge(x)", json!({ "x": [wide] }));
    assert_eq!(large - small, 999);

    // Pinned absolute counts; the wasm run must reproduce them.
    assert_eq!(cost_with("flatten(x)", json!({ "x": [big] })), FLATTEN_1000);
    assert_eq!(
        cost_with(
            "mergeDeep(x)",
            json!({ "x": [{ "a": { "b": [1, 2, 3] } }] })
        ),
        MERGE_DEEP_SMALL
    );
}

const FLATTEN_1000: u64 = 1003;
const MERGE_DEEP_SMALL: u64 = 9;

/// The recursive size walk for `mergeDeep` is itself bounded by the remaining budget.
#[test]
fn deep_walk_is_bounded_by_the_budget() {
    let deep: Vec<serde_json::Value> = (0..10_000).map(|i| json!({ "v": [i, i] })).collect();
    let meter = Meter::new(100);
    let err = Isolate::with_environment(json!({ "x": [{ "d": deep }] }).into())
        .with_meter(Some(meter.clone()))
        .run_standard("mergeDeep(x)")
        .unwrap_err();
    let exhausted = exhausted(err);
    assert_eq!(exhausted.limit, 100);
    // The walk stopped just past the remaining budget instead of visiting 50k values.
    assert!(exhausted.used <= 2 * 100 + 2, "{exhausted:?}");
}

/// Measuring a one-level builtin's cost must itself be bounded by the budget: a
/// 1M-element input against a budget of 100 fails after O(budget) scan steps, not after
/// walking all 1M children. `Meter::scan_steps` counts the values visited while
/// measuring; it is deterministic, so the same counts hold on wasm.
#[test]
fn nested_size_scan_is_bounded_by_the_budget() {
    const N: usize = 1_000_000;
    let cases = [
        // 1M children: the outer length alone exceeds the budget, so no child is scanned.
        ("flatten(x)", json!({ "x": vec![vec![1u8]; N] }), 0),
        ("merge(x)", json!({ "x": vec![json!({ "k": 1 }); N] }), 0),
        // 60 children of 10: the outer length fits, the scan stops at the 4th child.
        ("flatten(x)", json!({ "x": vec![vec![1u8; 10]; 60] }), 4),
    ];
    for (expression, env, max_steps) in cases {
        let meter = Meter::new(100);
        let err = Isolate::with_environment(env.into())
            .with_meter(Some(meter.clone()))
            .run_standard(expression)
            .unwrap_err();
        let exhausted = exhausted(err);
        assert_eq!(exhausted.limit, 100, "{expression}");
        // Clamped to cap + 1, so the reported total does not depend on the input size.
        assert_eq!(exhausted.used, NESTED_EXHAUSTED_AT, "{expression}");
        assert!(
            meter.scan_steps() <= max_steps,
            "{expression}: {} scan steps",
            meter.scan_steps()
        );
    }
}

const NESTED_EXHAUSTED_AT: u64 = 102;
