#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
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

//! T1-EXPR-4 (#2522): the structural bounds and the static cost bound of
//! `norn-expr/4`, and the always-metered evaluation budget.
//!
//! Over-bound source, depth, node, literal, closure-nesting, and
//! nested-iteration fixtures are rejected with stable codes; the estimate is
//! exact for straight-line literal expressions (so the per-opcode model
//! matches zen's compiler) and reported in the rejection message. The
//! generated soundness checks in `tests/check.rs` and `tests/corpus.rs`
//! evaluate every admitted expression under a budget of exactly its estimate.

mod support;

use std::collections::BTreeMap;

use norn_expr::check::{StructuralLimits, codes};
use norn_expr::{
    CostEstimate, EnvOptions, EvaluationBudget, ExprError, Expression, Number, RootCostBounds,
    Type, UnaryTest, Value, ZenTypeEnv, check_expression_with, check_expression_with_root_bounds,
    check_unary_with,
};
use norn_ir::ResourceLimitProfile;
use support::{env_for, roomy_limits, within_slice_limits};

const SKU: &str = "retail.Sku";
const ORDER: &str = "commerce.SalesOrder";

fn limits() -> ResourceLimitProfile {
    ResourceLimitProfile::norn_limits_v1()
}

fn estimate(self_type: &str, source: &str) -> Result<CostEstimate, Vec<&'static str>> {
    let expr = Expression::compile(source).unwrap_or_else(|err| panic!("{source}: {err}"));
    check_expression_with(&expr, &env_for(self_type), &Type::Json, &limits())
        .map(|checked| checked.cost())
        .map_err(|diagnostics| diagnostics.iter().map(|d| d.code).collect())
}

fn independent_roots_environment() -> ZenTypeEnv {
    ZenTypeEnv::input_only_with_records(
        BTreeMap::from([
            (
                "test.Slice".to_owned(),
                BTreeMap::from([("body".to_owned(), Type::text())]),
            ),
            (
                "test.Item".to_owned(),
                BTreeMap::from([("ok".to_owned(), Type::Bool)]),
            ),
        ]),
        BTreeMap::from([
            ("body".to_owned(), Type::text()),
            (
                "items".to_owned(),
                Type::Record("test.Item".to_owned()).list(),
            ),
        ]),
        EnvOptions::default(),
    )
    .expect("input environment")
    .bind_operand("slice", Type::Record("test.Slice".to_owned()))
    .expect("slice")
    .bind_operand("body", Type::text())
    .expect("flat alias")
}

fn independent_root_bounds() -> BTreeMap<String, RootCostBounds> {
    let limits = limits();
    let input = RootCostBounds {
        max_canonical_bytes: limits.envelope.max_output_bytes,
        max_records: u64::MAX,
    };
    BTreeMap::from([
        ("input".to_owned(), input),
        ("body".to_owned(), input),
        (
            "slice".to_owned(),
            RootCostBounds {
                max_canonical_bytes: limits.slice.max_canonical_bytes,
                max_records: u64::from(limits.slice.max_records),
            },
        ),
    ])
}

#[test]
fn empty_root_bounds_preserve_the_default_context_cost() {
    let env = independent_roots_environment();
    for source in ["len(input.body)", "count(input.items, #.ok)"] {
        let expression = Expression::compile(source).expect("compile");
        let legacy = check_expression_with(&expression, &env, &Type::Json, &limits())
            .expect("default bound")
            .cost()
            .units();
        let empty = check_expression_with_root_bounds(
            &expression,
            &env,
            &Type::Json,
            &limits(),
            &BTreeMap::new(),
        )
        .expect("same default bound")
        .cost()
        .units();
        assert_eq!(legacy, empty, "{source}");
    }
}

#[test]
fn input_only_cost_is_unchanged_by_an_unrelated_slice_root() {
    let expression = Expression::compile(
        "len(input.body) + len(input.body) + len(input.body) + len(input.body) > 0",
    )
    .expect("compile");
    let input_only = ZenTypeEnv::input_only(
        BTreeMap::from([("body".to_owned(), Type::text())]),
        EnvOptions::default(),
    )
    .expect("input");
    let bounds = independent_root_bounds();
    let check = |env: &ZenTypeEnv| {
        check_expression_with_root_bounds(&expression, env, &Type::Bool, &limits(), &bounds)
            .expect("four input reads fit decide fuel")
            .cost()
            .units()
    };
    assert_eq!(check(&input_only), check(&independent_roots_environment()));
}

#[test]
fn independent_root_costs_bound_near_cap_reads_and_derived_values() {
    let env = independent_roots_environment();
    let bounds = independent_root_bounds();
    let input = Value::object([("body", Value::from("i".repeat(32 * 1024 - 64)))]);
    let slice = Value::object([("body", Value::from("s".repeat(256 * 1024 - 64)))]);
    assert!(canonical_bytes(&input) <= 32 * 1024);
    assert!(canonical_bytes(&slice) <= 256 * 1024);
    let context = Value::object([("input", input), ("slice", slice)]);
    for source in [
        "len(input.body) + len(slice.body)",
        "input.body + slice.body",
        "{a: input, b: slice}",
        "true ? input.body : slice.body",
        "map([1, 2], input.body + slice.body)",
    ] {
        let expression = Expression::compile(source).expect("compile");
        let bound = check_expression_with_root_bounds(
            &expression,
            &env,
            &Type::Json,
            &roomy_limits(),
            &bounds,
        )
        .expect("check")
        .cost()
        .units();
        let used = expression
            .evaluate(&context, EvaluationBudget::new(bound))
            .unwrap_or_else(|error| panic!("{source}: {error}; bound {bound}"))
            .units_used();
        assert!(used <= bound, "{source}: metered {used}, bound {bound}");
        if source == "len(input.body) + len(slice.body)" {
            let pooled = check_expression_with(&expression, &env, &Type::Json, &limits())
                .expect("legacy pooled context")
                .cost()
                .units();
            assert!(used > pooled, "independent roots must add their bytes");
        }
    }
}

#[test]
fn input_alias_reads_are_independent_and_still_obey_decide_fuel() {
    let env = independent_roots_environment();
    let bounds = independent_root_bounds();
    let repeated = |root| vec![format!("len({root})"); 16].join(" + ");
    let source = format!(
        "({}) > 0 and ({}) > 0",
        repeated("input.body"),
        repeated("body")
    );
    let expression = Expression::compile(&source).expect("compile");
    let findings =
        check_expression_with_root_bounds(&expression, &env, &Type::Bool, &limits(), &bounds)
            .expect_err("duplicated input aliases exceed decide fuel");
    assert!(
        findings
            .iter()
            .any(|d| d.code == codes::COST_EXCEEDS_BUDGET)
    );
    let body = Value::from("i".repeat(32 * 1024 - 64));
    let input = Value::object([("body", body.clone())]);
    assert!(canonical_bytes(&input) <= 32 * 1024);
    let context = Value::object([("input", input), ("body", body)]);
    let bound =
        check_expression_with_root_bounds(&expression, &env, &Type::Bool, &roomy_limits(), &bounds)
            .expect("roomy proof")
            .cost()
            .units();
    let used = expression
        .evaluate(&context, EvaluationBudget::new(bound))
        .expect("metered within independent-root proof")
        .units_used();
    assert!(used > limits().decide.fuel_units);
    assert!(used <= bound);
}

#[test]
fn input_record_lists_use_their_byte_cap_in_closures_and_result_copies() {
    let env = independent_roots_environment();
    let bounds = independent_root_bounds();
    let input = Value::object([(
        "items",
        Value::Array(vec![Value::object([("ok", Value::Bool(true))]); 2_000]),
    )]);
    assert!(canonical_bytes(&input) <= 32 * 1024);
    let context = Value::object([("input", input)]);
    for source in [
        "count(input.items, #.ok)",
        "map(input.items, #.ok)",
        "filter(input.items, #.ok)",
    ] {
        let expression = Expression::compile(source).expect("compile");
        let bound =
            check_expression_with_root_bounds(&expression, &env, &Type::Json, &limits(), &bounds)
                .expect("byte-bounded input list")
                .cost()
                .units();
        let used = expression
            .evaluate(&context, EvaluationBudget::new(bound))
            .unwrap_or_else(|error| panic!("{source}: {error}; bound {bound}"))
            .units_used();
        assert!(used <= bound, "{source}: metered {used}, bound {bound}");
        if source == "count(input.items, #.ok)" {
            let slice_record_bound =
                check_expression_with(&expression, &env, &Type::Json, &limits())
                    .expect("default slice record cap")
                    .cost()
                    .units();
            assert!(
                used > slice_record_bound,
                "input allows more than 1,000 records"
            );
        }
    }
}

#[test]
fn record_list_bounds_remain_sound_after_a_conditional_loses_root_provenance() {
    let records = BTreeMap::from([
        (
            "test.Item".to_owned(),
            BTreeMap::from([("ok".to_owned(), Type::Bool)]),
        ),
        (
            "test.Group".to_owned(),
            BTreeMap::from([(
                "items".to_owned(),
                Type::Record("test.Item".to_owned()).list(),
            )]),
        ),
    ]);
    let env = ZenTypeEnv::input_only_with_records(
        records,
        BTreeMap::from([
            ("group".to_owned(), Type::Record("test.Group".to_owned())),
            ("other".to_owned(), Type::Record("test.Group".to_owned())),
        ]),
        EnvOptions::default(),
    )
    .expect("group environment");
    let input = Value::object([
        (
            "group",
            Value::object([(
                "items",
                Value::Array(vec![Value::object([("ok", Value::Bool(true))]); 2_000]),
            )]),
        ),
        ("other", Value::object([("items", Value::Array(vec![]))])),
    ]);
    assert!(canonical_bytes(&input) <= 32 * 1024);
    let context = Value::object([("input", input)]);
    let expression = Expression::compile("count((true ? input.group : input.other).items, #.ok)")
        .expect("compile");
    let bound = check_expression_with_root_bounds(
        &expression,
        &env,
        &Type::Json,
        &roomy_limits(),
        &independent_root_bounds(),
    )
    .expect("conservative bound after origin loss")
    .cost()
    .units();
    let used = expression
        .evaluate(&context, EvaluationBudget::new(bound))
        .expect("input record list exceeds slice record count but fits proof")
        .units_used();
    assert!(used <= bound);
}

fn rejects(self_type: &str, source: &str, code: &str) {
    match estimate(self_type, source) {
        Ok(cost) => panic!("`{source}` must be rejected with {code}, admitted at {cost}"),
        Err(found) => assert!(
            found.contains(&code),
            "`{source}`: expected {code}, got {found:?}"
        ),
    }
}

#[test]
fn bound_codes_are_registered() {
    for code in [
        codes::BOUND_SOURCE,
        codes::BOUND_NODES,
        codes::BOUND_DEPTH,
        codes::BOUND_LITERAL,
        codes::BOUND_CLOSURE_DEPTH,
        codes::COST_EXCEEDS_BUDGET,
    ] {
        assert!(codes::ALL.contains(&code), "{code}");
    }
    let bounds = StructuralLimits::NORN_EXPR_4;
    assert_eq!(bounds.max_source_bytes, 16_384);
    assert_eq!(bounds.max_nodes, 4096);
    assert_eq!(bounds.max_depth, 128);
    assert_eq!(bounds.max_string_literal_bytes, 4096);
    assert_eq!(bounds.max_literal_items, 1024);
}

#[test]
fn over_bound_expressions_are_rejected_with_stable_codes() {
    // Source length and bracket nesting, before zen parses: compile errors
    // carrying the bound's code.
    let long = format!("'{}' == 'x'", "a".repeat(20_000));
    compile_rejects(&long, codes::BOUND_SOURCE);
    let nested = format!("{}1{}", "(".repeat(200), ")".repeat(200));
    compile_rejects(&nested, codes::BOUND_DEPTH);
    // AST depth without brackets: a chain of 200 `not`s.
    let chain = format!("{}true", "not ".repeat(200));
    rejects(SKU, &chain, codes::BOUND_DEPTH);
    // Node count: five 1,000-element lists are 5,006 nodes, 3 deep.
    let row = format!("[{}]", vec!["1"; 1_000].join(","));
    let wide = format!("[{}]", [row.as_str(); 5].join(","));
    rejects(SKU, &wide, codes::BOUND_NODES);
    // Literal sizes.
    let string = format!("'{}'", "s".repeat(5_000));
    rejects(SKU, &string, codes::BOUND_LITERAL);
    let list = format!("[{}]", vec!["1"; 1_100].join(","));
    rejects(SKU, &list, codes::BOUND_LITERAL);
    // Closure nesting beyond the slice relation depth (3).
    rejects(
        SKU,
        "some([1], some([1], some([1], some([1], # > 0))))",
        codes::BOUND_CLOSURE_DEPTH,
    );
    assert!(estimate(SKU, "some([1], some([1], some([1], # > 0)))").is_ok());
    // Just within the bounds.
    let at_limit = format!("[{}]", vec!["1"; 1_024].join(","));
    assert!(estimate(SKU, &at_limit).is_ok());
    let at_depth = format!("{}1{}", "(".repeat(100), ")".repeat(100));
    assert!(estimate(SKU, &at_depth).is_ok());
}

fn compile_rejects(source: &str, code: &str) {
    match Expression::compile(source) {
        Err(ExprError::Compile { message }) => assert!(message.contains(code), "{message}"),
        other => panic!("expected a compile error with {code}, got {other:?}"),
    }
}

/// #2852 risk: zen parses and compiles recursively, and so does the checker.
/// Over-deep sources are rejected before either recurses.
#[test]
fn deep_sources_are_rejected_before_anything_recurses() {
    let env = env_for(SKU);
    let nested = format!("{}1{}", "(".repeat(50_000), ")".repeat(50_000));
    match UnaryTest::compile(&nested) {
        Err(ExprError::Compile { message }) => {
            assert!(message.contains(codes::BOUND_SOURCE), "{message}");
        }
        other => panic!("expected a compile error, got {other:?}"),
    }
    let nested = format!("{}1{}", "(".repeat(5_000), ")".repeat(5_000));
    compile_rejects(&nested, codes::BOUND_DEPTH);
    assert!(UnaryTest::compile(&nested).is_err());
    assert!(norn_expr::check_unary_syntax(&nested).is_err());
    // `a, b, c, ...`: zen joins the parts left-deep.
    let parts = vec!["1"; 300].join(", ");
    let test = UnaryTest::compile(&parts).expect("compile");
    let found = check_unary_with(&test, &env, &Type::Number, &limits()).expect_err("reject");
    assert!(
        found.iter().any(|d| d.code == codes::BOUND_DEPTH),
        "{found:?}"
    );
}

/// Nested iteration multiplies (zen adoption review F3): two closures over
/// to-many links are 1,000 x 1,000 iterations, and a closure over a scalar
/// slice list may run as often as the slice has room for elements.
#[test]
fn nested_iteration_over_budget_is_rejected_with_the_estimate() {
    for (self_type, source) in [
        (
            ORDER,
            "flatMap(self.lines as l, map(l.item.order_lines, #.quantity))",
        ),
        (
            ORDER,
            "some(self.customer.orders, all(#.lines, #.quantity > 0))",
        ),
        (
            ORDER,
            "map(self.lines as l, count(actor.roles, # == l.item.name))",
        ),
        (SKU, "all(actor.roles, # != 'guest')"),
        // Cheap to run (each element is a handle to one list) but its result
        // is |roles|^2 values to copy out.
        (SKU, "map(actor.roles, actor.roles)"),
        (
            SKU,
            "sum(flatMap(self.order_lines as l, map(actor.roles, l.quantity)))",
        ),
    ] {
        rejects(self_type, source, codes::COST_EXCEEDS_BUDGET);
        // Well-typed all the same.
        let expr = Expression::compile(source).expect("compile");
        let checked =
            check_expression_with(&expr, &env_for(self_type), &Type::Json, &roomy_limits())
                .unwrap_or_else(|d| panic!("`{source}` should type-check: {d:?}"));
        assert!(checked.cost().units() > limits().decide.fuel_units);
    }
    let expr = Expression::compile("all(actor.roles, # != 'guest')").expect("compile");
    let diagnostics =
        check_expression_with(&expr, &env_for(SKU), &Type::Bool, &limits()).expect_err("reject");
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0].message.contains("units") && diagnostics[0].message.contains("1000000"),
        "the message reports the estimate and the budget: {}",
        diagnostics[0].message
    );
}

/// Common single closures over to-many links stay within the budget: the
/// iteration count is the record cap, and each record's strings are distinct
/// positions of the slice.
#[test]
fn single_closures_over_links_are_admitted() {
    for (self_type, source) in [
        (ORDER, "some(self.lines, #.quantity > 1)"),
        (ORDER, "sum(map(self.lines, #.amount))"),
        (ORDER, "count(self.lines, #.item.name == 'x')"),
        (ORDER, "some(self.lines, contains(lower(#.item.name), 'x'))"),
        (ORDER, "filter(self.customer.orders, len(#.lines) > 2)"),
        (SKU, "'admin' in actor.roles"),
        (SKU, "self.name + ' ' + self.sku_code"),
    ] {
        let cost = estimate(self_type, source)
            .unwrap_or_else(|codes| panic!("`{source}` should be admitted, got {codes:?}"));
        assert!(cost.units() <= limits().decide.fuel_units);
    }
    // The estimate follows the profile: a smaller slice admits more.
    let mut small = limits();
    small.slice.max_records = 10;
    let expr = Expression::compile("some(self.customer.orders, all(#.lines, #.quantity > 0))")
        .expect("compile");
    assert!(check_expression_with(&expr, &env_for(ORDER), &Type::Bool, &small).is_ok());
}

fn measure(source: &str) -> (u64, u64) {
    let expr = Expression::compile(source).expect("compile");
    let checked = check_expression_with(&expr, &env_for(SKU), &Type::Json, &limits())
        .unwrap_or_else(|d| panic!("`{source}`: {d:?}"));
    let context = Value::object::<String>([]);
    let evaluation = expr
        .evaluate(&context, EvaluationBudget::new(checked.cost().units()))
        .unwrap_or_else(|err| panic!("`{source}`: {err}"));
    (evaluation.units_used(), checked.cost().units())
}

/// On straight-line literal expressions (every branch and loop exit taken the
/// way the estimate assumes) the estimate is exactly the metered cost: the
/// per-opcode model matches zen's compiler and cost model.
#[test]
fn estimate_is_exact_on_straight_line_literals() {
    for source in [
        "1 + 2",
        "-1",
        "not true",
        "1 != 2",
        "[1, 2, 3]",
        "'ab' + 'c'",
        "'ab' == 'ab'",
        "'a' in ['a', 'bc']",
        "2 not in [1, 3]",
        "2 in [1..3]",
        "true ? 1 : 2",
        "true and true",
        "false or true",
        "({a: 1}).a",
        "{ab: [1], c: 'x'}",
        "[1, 2][1]",
        "len('abc')",
        "len([1, 2]) + 1",
        "upper('\u{390}') == 'x'",
        "round(1.25, 1)",
        "sum([1, 2, 3])",
        "contains('abc', 'b')",
        "date('2024-02-29')",
        "timestamp('2026-01-01T00:00:00.123456789Z')",
        "days_between(date('2024-02-28'), date('2024-03-01'))",
        "add_days(date('2024-02-29'), 1)",
        "seconds_between(timestamp('2026-01-01T00:00:00.125Z'), timestamp('2026-01-01T00:00:01.375Z'))",
        "[date('2024-02-29')]",
        "{at: timestamp('2026-01-01T00:00:00.123456789Z')}",
        "map([date('2024-02-29')], #)",
        "map([1, 2, 3], # * 2)",
        "filter([1, 2, 3], # > 0)",
        "count([1, 2], # > 0)",
        "all([1, 2], # > 0)",
        "none([1, 2], # > 5)",
        "some([1, 2], # > 5)",
        "one([2, 3], # > 1)",
        "flatMap([1, 2], [#, #])",
        "some([1, 2] as x, some([3], # > x + 5))",
    ] {
        let (used, bound) = measure(source);
        assert_eq!(used, bound, "`{source}`: metered {used}, estimated {bound}");
    }
}

/// Timestamp wire output has a conservative 30-byte copy bound, while
/// canonical AutoSi formatting emits 20, 24, 27, or 30 bytes. These variable
/// widths and a second-boundary carry must stay below the static estimate.
#[test]
fn temporal_timestamp_copy_estimate_bounds_canonical_widths() {
    for (source, expected) in [
        ("timestamp('2026-01-01T00:00:00Z')", "2026-01-01T00:00:00Z"),
        (
            "timestamp('2026-01-01T00:00:00.123Z')",
            "2026-01-01T00:00:00.123Z",
        ),
        (
            "timestamp('2026-01-01T00:00:00.123456Z')",
            "2026-01-01T00:00:00.123456Z",
        ),
        (
            "timestamp('2026-01-01T00:00:00.123456789Z')",
            "2026-01-01T00:00:00.123456789Z",
        ),
        (
            "timestamp('2026-01-01T00:00:00.123000000Z')",
            "2026-01-01T00:00:00.123Z",
        ),
        (
            "add_seconds(timestamp('2026-01-01T00:00:00.125Z'), 2)",
            "2026-01-01T00:00:02.125Z",
        ),
        (
            "add_seconds(timestamp('2026-01-01T00:00:00.999Z'), 1)",
            "2026-01-01T00:00:01.999Z",
        ),
    ] {
        let expr = Expression::compile(source).expect("compile");
        let bound = check_expression_with(&expr, &env_for(SKU), &Type::Json, &limits())
            .unwrap_or_else(|d| panic!("`{source}`: {d:?}"))
            .cost()
            .units();
        let context = Value::object::<String>([]);
        let evaluation = expr
            .evaluate(&context, EvaluationBudget::new(bound))
            .unwrap_or_else(|err| panic!("`{source}` (estimate {bound}): {err}"));
        assert_eq!(evaluation.value(), &Value::from(expected), "`{source}`");
        assert!(
            evaluation.units_used() <= bound,
            "`{source}`: metered {}, estimated {bound}",
            evaluation.units_used()
        );
    }
}

/// Where a branch, short-circuit, or loop exits early the estimate is an
/// upper bound, never below the metered cost.
#[test]
fn estimate_bounds_early_exits() {
    for source in [
        "false ? 1 : 2",
        "false and true",
        "true or false",
        "filter([1, 2, 3], # > 1)",
        "all([1, 2], # > 1)",
        "some([1, 2], # > 0)",
        "one([1, 2], # > 1)",
        "none([1, 2], # > 0)",
        "count([1, 2], # > 1)",
        "lower('AB') + 'c'",
        "map([[1], [2, 3]], len(#))",
        "trim(' a ')",
    ] {
        let (used, bound) = measure(source);
        assert!(
            used <= bound,
            "`{source}`: metered {used}, estimated {bound}"
        );
    }
}

/// The estimate bounds slice reads at the byte cap: one 240 KB string read
/// twice, lower-casing that grows it by half (`İ`, 2 bytes, lower-cases to
/// 3), and a long result, whose copy out of the evaluator is charged.
#[test]
fn estimate_bounds_max_size_slice_strings() {
    let name = "\u{130}".repeat(120_000);
    let context = Value::object([(
        "self",
        Value::object([
            ("sku_code", Value::from("S")),
            ("name", Value::from(name.as_str())),
            ("price", Value::Number(Number::from(1))),
            ("weight", Value::Null),
            ("status", Value::from("draft")),
            ("launched_at", Value::from("2024-02-29T00:00:00Z")),
            ("launch_date", Value::Null),
            ("shelf_life", Value::from("PT1S")),
            ("is_taxable", Value::Bool(true)),
            ("stock", Value::from(1_i64)),
            ("order_lines", Value::Array(Vec::new())),
        ]),
    )]);
    assert!(within_slice_limits(&context));
    for source in [
        "self.name == self.name",
        "len(lower(self.name))",
        "len(split(self.name, '')) - 2",
        "contains(lower(self.name), self.sku_code)",
        "lower(self.name)",
    ] {
        let expr = Expression::compile(source).expect("compile");
        let bound = check_expression_with(&expr, &env_for(SKU), &Type::Json, &limits())
            .unwrap_or_else(|d| panic!("`{source}`: {d:?}"))
            .cost()
            .units();
        let used = expr
            .evaluate(&context, EvaluationBudget::new(bound))
            .unwrap_or_else(|err| panic!("`{source}` (estimate {bound}): {err}"))
            .units_used();
        assert!(
            used <= bound,
            "`{source}`: metered {used}, estimated {bound}"
        );
        assert!(
            used > 200_000,
            "`{source}` should read the long name: {used}"
        );
    }
}

/// There is no unmetered evaluation: the default budget is the decide fuel,
/// and an (unchecked) expression that needs more fails with the typed error.
#[test]
fn evaluation_is_always_metered_with_the_decide_budget() {
    assert_eq!(
        EvaluationBudget::default(),
        EvaluationBudget::decide(&ResourceLimitProfile::norn_limits_v1())
    );
    assert_eq!(EvaluationBudget::default().units(), 1_000_000);
    let expr = Expression::compile("sum(flatMap(xs, map(xs, # * 2)))").expect("compile");
    let xs: Vec<Value> = (0..1_000_i64).map(Value::from).collect();
    let context = Value::object([("xs", Value::Array(xs))]);
    match expr.evaluate(&context, EvaluationBudget::default()) {
        Err(ExprError::BudgetExhausted { limit, used }) => {
            assert_eq!(limit, 1_000_000);
            assert!(used > limit);
        }
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
    // Structure sharing: 4,000 iterations cost ~16,000 units, but the result
    // holds 16,000,000 values. Copying it out is metered, so it fails before
    // allocating.
    let expr = Expression::compile("map(xs, xs)").expect("compile");
    let xs: Vec<Value> = (0..4_000_i64).map(Value::from).collect();
    let context = Value::object([("xs", Value::Array(xs))]);
    assert!(matches!(
        expr.evaluate(&context, EvaluationBudget::default()),
        Err(ExprError::BudgetExhausted { .. })
    ));
}

#[test]
fn unary_tests_report_their_estimate() {
    let env = env_for(SKU);
    let test = UnaryTest::compile("> 5, < 1").expect("compile");
    let bound = check_unary_with(&test, &env, &Type::Number, &limits()).expect("admit");
    let matched = test
        .evaluate(
            &Value::object::<String>([]),
            &Value::Number(Number::from(3)),
            EvaluationBudget::new(bound.units()),
        )
        .expect("within the estimate");
    assert!(!matched);
    let wildcard = UnaryTest::compile("").expect("wildcard");
    assert_eq!(
        check_unary_with(&wildcard, &env, &Type::Json, &limits())
            .expect("wildcard")
            .units(),
        0
    );
    // A closure over a list subject as large as the slice allows.
    let test = UnaryTest::compile("some($, # > 1)").expect("compile");
    let found = check_unary_with(&test, &env, &Type::Number.list(), &limits())
        .expect_err("131,071 numbers fit in 256 KiB");
    assert_eq!(found[0].code, codes::COST_EXCEEDS_BUDGET);
}

fn canonical_bytes(value: &Value) -> usize {
    use norn_ir::canonical::{JsonValue, serialize};
    fn json(value: &Value) -> JsonValue {
        match value {
            Value::Null => JsonValue::Null,
            Value::Bool(b) => JsonValue::Bool(*b),
            Value::Number(n) => JsonValue::Number(n.as_str().to_owned()),
            Value::String(s) => JsonValue::String(s.clone()),
            Value::Array(items) => JsonValue::Array(items.iter().map(json).collect()),
            Value::Object(entries) => {
                JsonValue::Object(entries.iter().map(|(k, v)| (k.clone(), json(v))).collect())
            }
        }
    }
    serialize(&json(value)).expect("canonical").len()
}

fn actor_with(auth_method: &str, roles: Vec<Value>) -> Value {
    Value::object([(
        "actor",
        Value::object([
            ("actorKind", Value::from("user")),
            ("subjectId", Value::from("u")),
            ("agentSessionId", Value::Null),
            ("capability", Value::from("c")),
            ("roles", Value::Array(roles)),
            ("authMethod", Value::from(auth_method)),
            (
                "scope",
                Value::object([("orgId", Value::from("o")), ("tenantId", Value::Null)]),
            ),
        ]),
    )])
}

fn bound_and_use(source: &str, context: &Value) -> (u64, u64) {
    let expr = Expression::compile(source).expect("compile");
    let bound = check_expression_with(&expr, &env_for(SKU), &Type::Json, &limits())
        .unwrap_or_else(|d| panic!("`{source}`: {d:?}"))
        .cost()
        .units();
    let used = expr
        .evaluate(context, EvaluationBudget::new(bound))
        .unwrap_or_else(|err| panic!("`{source}` (estimate {bound}): {err}"))
        .units_used();
    (used, bound)
}

/// Sol review of #2869 (critical): Norn1 canonicalization NFC-normalizes
/// strings, so a slice within the canonical byte cap can carry longer raw
/// strings (`e` + U+0301 is 3 bytes, NFC `é` is 2). The evaluator reads its
/// inputs in NFC, the canonical form, so the byte premise holds.
#[test]
fn evaluator_inputs_are_nfc_normalized_so_canonical_bytes_bound_them() {
    let decomposed = "e\u{301}".repeat(100_000);
    let context = actor_with(&decomposed, Vec::new());
    let canonical = canonical_bytes(&context);
    assert!(
        canonical as u64 <= limits().slice.max_canonical_bytes,
        "{canonical} canonical bytes"
    );
    assert!(
        decomposed.len() > canonical,
        "raw bytes exceed canonical bytes"
    );
    let (used, bound) = bound_and_use("actor.authMethod == actor.authMethod", &context);
    assert!(used <= bound, "metered {used}, estimated {bound}");
    assert!(
        used > 400_000,
        "reads the 200,000-byte NFC string twice: {used}"
    );
    // The evaluator sees the NFC form.
    let expr = Expression::compile("actor.authMethod").expect("compile");
    let value = expr
        .evaluate(&context, EvaluationBudget::default())
        .expect("evaluate")
        .into_value();
    assert_eq!(value, Value::from("\u{e9}".repeat(100_000)));
}

/// Sol review of #2869: every copied value is charged, empty strings too.
#[test]
fn result_copies_charge_every_value() {
    let expr = Expression::compile("map(xs, xs)").expect("compile");
    let xs: Vec<Value> = vec![Value::from(""); 1_000];
    let context = Value::object([("xs", Value::Array(xs))]);
    // ~4,000 VM units, but 1,000 lists of 1,000 values to copy out.
    assert!(matches!(
        expr.evaluate(&context, EvaluationBudget::default()),
        Err(ExprError::BudgetExhausted { .. })
    ));
    let roles = vec![Value::from(""); 60_000];
    let context = actor_with("token", roles);
    assert!(canonical_bytes(&context) as u64 <= limits().slice.max_canonical_bytes);
    let (used, bound) = bound_and_use("actor.roles", &context);
    assert!(used > 60_000, "one unit per copied empty string: {used}");
    assert!(used <= bound, "metered {used}, estimated {bound}");
}

/// Sol review of #2869 (high): zen's parser and compiler recurse once per
/// postfix, prefix, or binary operator, not only per bracket. Sources that
/// could nest too deep are rejected before zen parses them, and everything
/// the source bound lets through is parsed, compiled, and rejected by the AST
/// bound without exhausting a default 2 MiB thread stack.
#[test]
fn recursive_syntax_is_bounded_before_parsing_on_a_small_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let chain = format!("a{}", ".a".repeat(8_191));
            assert_eq!(chain.len(), 16_383);
            compile_rejects(&chain, codes::BOUND_DEPTH);
            compile_rejects(&format!("{}1", "-".repeat(2_000)), codes::BOUND_DEPTH);
            compile_rejects(&format!("{}true", "not ".repeat(1_000)), codes::BOUND_DEPTH);
            compile_rejects(&vec!["1"; 2_000].join(" + "), codes::BOUND_DEPTH);
            // Postfix chains: computed access, slices, mixed member/index/call
            // (Sol review of 4dc02699: `a` + `[0]`x5000 overflowed).
            compile_rejects(&format!("a{}", "[0]".repeat(5_000)), codes::BOUND_DEPTH);
            // Sol review of fcd88b2a: the count is stateless, so closing
            // brackets (zen's interval parser probes them) and characters a
            // quote or escape would hide are counted too.
            compile_rejects(&format!("{}0..1[", "]".repeat(5_000)), codes::BOUND_DEPTH);
            compile_rejects(&format!("`'` + {}1", "-".repeat(600)), codes::BOUND_DEPTH);
            compile_rejects(
                &format!("'\\\\' + {}1", "-".repeat(600)),
                codes::BOUND_DEPTH,
            );
            compile_rejects(&format!("'\\'' + {}1", "-".repeat(600)), codes::BOUND_DEPTH);
            compile_rejects(&format!("a{}", "[0:1]".repeat(3_000)), codes::BOUND_DEPTH);
            compile_rejects(
                &format!("a{}", ".b[0](1)".repeat(1_500)),
                codes::BOUND_DEPTH,
            );
            compile_rejects(
                &format!("{}1{}", "abs(".repeat(120), ")".repeat(120)).repeat(5),
                codes::BOUND_DEPTH,
            );
            let parts = vec!["1"; 2_000].join(", ");
            assert!(UnaryTest::compile(&parts).is_err());
            assert!(norn_expr::check_unary_syntax(&parts).is_err());
            // Just within the source bound: zen parses and compiles them, the
            // AST bound rejects them.
            let env = env_for(SKU);
            for source in [
                format!("self{}", ".a".repeat(500)),
                format!("{}1", "-".repeat(500)),
                format!("{}true", "not ".repeat(500)),
                vec!["1"; 250].join(" + "),
                format!("{}1{}", "(".repeat(128), ")".repeat(128)),
                format!("self{}", "[0]".repeat(250)),
                format!("self{}", "[0:1]".repeat(150)),
                format!("self{}", ".a[0]".repeat(160)),
            ] {
                let expr = Expression::compile(&source).unwrap_or_else(|err| {
                    let head: String = source.chars().take(20).collect();
                    panic!("{head}...: {err}")
                });
                let found = check_expression_with(&expr, &env, &Type::Json, &limits())
                    .expect_err("over the AST depth");
                assert!(
                    found.iter().any(|d| d.code == codes::BOUND_DEPTH),
                    "{found:?}"
                );
            }
            let parts = vec!["1"; 500].join(", ");
            let test = UnaryTest::compile(&parts).expect("compile");
            let found = check_unary_with(&test, &env, &Type::Number, &limits())
                .expect_err("over the AST depth");
            assert!(
                found.iter().any(|d| d.code == codes::BOUND_DEPTH),
                "{found:?}"
            );
        })
        .expect("spawn");
    handle.join().expect("no stack overflow");
}

#[test]
fn scalar_split_cost_bounds_result_copy_and_iteration() {
    for text in ["".to_owned(), "ع".repeat(4096), "😀".repeat(60_000)] {
        let context = Value::object([("self", Value::object([("name", Value::from(text))]))]);
        assert!(within_slice_limits(&context));
        for source in [
            "split(self.name, '')",
            "map(split(self.name, ''), upper(#))",
        ] {
            let expr = Expression::compile(source).expect("compile");
            let bound = check_expression_with(&expr, &env_for(SKU), &Type::Json, &roomy_limits())
                .unwrap_or_else(|d| panic!("`{source}`: {d:?}"))
                .cost()
                .units();
            let used = expr
                .evaluate(&context, EvaluationBudget::new(bound))
                .unwrap_or_else(|err| panic!("`{source}` (estimate {bound}): {err}"))
                .units_used();
            assert!(
                used <= bound,
                "`{source}`: metered {used}, estimated {bound}"
            );
        }
    }
}

#[test]
fn bounded_string_precharges_and_result_copies_fit_the_static_bound() {
    let name = "FASHION / SS24 / Élite 😀 ".repeat(2_000);
    let context = Value::object([(
        "self",
        Value::object([("name", Value::from(name.as_str()))]),
    )]);
    assert!(within_slice_limits(&context));

    for source in [
        "tokens(self.name, 'alnum', 24, 8)",
        "join(tokens(self.name, 'alnum', 24, 8), '-')",
        "take(upper(join(tokens(self.name, 'alnum', 24, 8), '-')), 128)",
        "map(tokens(self.name, 'alnum', 24, 8), take(#, 3))",
    ] {
        let expr = Expression::compile(source).expect("compile");
        let bound = check_expression_with(&expr, &env_for(SKU), &Type::Json, &limits())
            .unwrap_or_else(|diagnostics| panic!("`{source}`: {diagnostics:?}"))
            .cost()
            .units();
        let used = expr
            .evaluate(&context, EvaluationBudget::new(bound))
            .unwrap_or_else(|error| panic!("`{source}` (estimate {bound}): {error}"))
            .units_used();
        assert!(
            used <= bound,
            "`{source}`: metered {used}, estimated {bound}"
        );
    }
}

#[test]
fn bounded_string_normalization_example_is_composable() {
    let expr =
        Expression::compile("take(upper(join(tokens(self.name, 'alnum', 24, 32), '-')), 128)")
            .expect("compile");
    let context = Value::object([(
        "self",
        Value::object([("name", Value::from("Fancy / Yé! Dress—SS24 😀"))]),
    )]);
    let checked = check_expression_with(&expr, &env_for(SKU), &Type::text(), &limits())
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:?}"));
    let evaluation = expr
        .evaluate(&context, EvaluationBudget::new(checked.cost().units()))
        .expect("the static estimate covers evaluation and result traversal");
    assert_eq!(evaluation.value(), &Value::from("FANCY-Y-DRESS-SS24"));
    assert!(evaluation.units_used() <= checked.cost().units());
}
