//! Expression fixtures evaluated through the `norn-expr` wrapper (T1-EXPR-1).
//!
//! These run natively under `cargo test`. `wasm32-unknown-unknown` is covered
//! by the CI `wasm32` job building `norn-expr` for that target; the fork's own
//! CI runs the metering determinism tests on wasm32 with wasm-bindgen-test and
//! pins the same unit counts asserted below (zen-fork-ledger.md, "Evidence").

use norn_expr::{EvaluationBudget, ExprError, Expression, Number, Value};

const BUDGET: EvaluationBudget = EvaluationBudget::new(100_000);

fn num(text: &str) -> Value {
    match text.parse::<Number>() {
        Ok(n) => Value::Number(n),
        Err(err) => Value::String(format!("bad fixture number {text}: {err}")),
    }
}

fn eval(source: &str, context: &Value, budget: EvaluationBudget) -> Result<Value, ExprError> {
    Ok(Expression::compile(source)?
        .evaluate(context, budget)?
        .into_value())
}

fn context() -> Value {
    Value::object([
        ("price", num("12.50")),
        ("quantity", Value::from(3)),
        ("customer", Value::object([("tier", Value::from("gold"))])),
        (
            "lines",
            Value::Array(vec![Value::from(1), Value::from(2), Value::from(3)]),
        ),
        ("big", num("9007199254740993")),
        ("flag", Value::Bool(true)),
        ("missing", Value::Null),
    ])
}

#[test]
fn fixtures_evaluate() -> Result<(), ExprError> {
    let fixtures: &[(&str, Value)] = &[
        ("1 + 2 * 3", Value::from(7)),
        ("price * quantity", num("37.5")),
        ("0.1 + 0.2", num("0.3")),
        ("big + 1", num("9007199254740994")),
        ("customer.tier == 'gold'", Value::Bool(true)),
        (
            "customer.tier == 'gold' ? 'vip' : 'std'",
            Value::from("vip"),
        ),
        ("sum(lines)", Value::from(6)),
        (
            "map(lines, # * 2)",
            Value::Array(vec![Value::from(2), Value::from(4), Value::from(6)]),
        ),
        (
            "filter(lines, # > 1)",
            Value::Array(vec![Value::from(2), Value::from(3)]),
        ),
        ("len(lines)", Value::from(3)),
        ("missing ?? 'default'", Value::from("default")),
        ("flag and quantity > 2", Value::Bool(true)),
        ("quantity in [1..5]", Value::Bool(true)),
        ("upper(customer.tier)", Value::from("GOLD")),
        ("matches(customer.tier, '^g[a-z]+$')", Value::Bool(true)),
        (
            "{ total: price * quantity, tier: customer.tier }",
            Value::object([("total", num("37.5")), ("tier", Value::from("gold"))]),
        ),
        ("-0", Value::from(0)),
        ("nothing.here", Value::Null),
    ];
    let ctx = context();
    for (source, expected) in fixtures {
        let actual = eval(source, &ctx, BUDGET)?;
        assert_eq!(&actual, expected, "`{source}`");
    }
    Ok(())
}

#[test]
fn compiled_expression_is_reusable() -> Result<(), ExprError> {
    let expr = Expression::compile("x * 2")?;
    assert_eq!(expr.source(), "x * 2");
    for x in [0, 1, -7] {
        let ctx = Value::object([("x", Value::from(x))]);
        let doubled = x.checked_mul(2).map_or(Value::Null, Value::from);
        assert_eq!(expr.evaluate(&ctx, BUDGET)?.value(), &doubled);
    }
    Ok(())
}

#[test]
fn syntax_errors_are_compile_errors() {
    for source in ["1 +", "(", "foo(", "'unterminated"] {
        assert!(
            matches!(Expression::compile(source), Err(ExprError::Compile { .. })),
            "`{source}`"
        );
    }
}

#[test]
fn runtime_errors_are_evaluation_errors() {
    let err = eval("upper(5)", &Value::Null, BUDGET);
    assert!(matches!(err, Err(ExprError::Evaluation { .. })), "{err:?}");
}

#[test]
fn failed_evaluation_attempts_report_their_meter_usage() {
    let expression = Expression::compile("upper(5)").expect("compile");
    let context = Value::Null;
    let attempt = expression.evaluate_metered(&context, EvaluationBudget::new(100));
    assert!(matches!(&attempt.result, Err(ExprError::Evaluation { .. })));
    assert!(attempt.units_used > 0);

    // An ordinary evaluate remains the same error-returning convenience API.
    assert_eq!(
        expression.evaluate(&context, EvaluationBudget::new(100)),
        attempt.result
    );

    // If the caller carries the failed attempt's fuel forward, its next
    // evaluation gets only the remainder and exhausts its smaller budget
    // before it can repeat the late runtime error. A rejected metered
    // operation need not consume every unit left in that budget.
    let next_budget = attempt.units_used - 1;
    let next = expression.evaluate_metered(&context, EvaluationBudget::new(next_budget));
    assert!(matches!(
        &next.result,
        Err(ExprError::BudgetExhausted { .. })
    ));
    assert!(next.units_used > 0);
    assert!(next.units_used <= next_budget);

    let repeated = expression.evaluate_metered(&context, EvaluationBudget::new(next_budget));
    assert_eq!(repeated.result, next.result);
    assert_eq!(repeated.units_used, next.units_used);
}

#[test]
fn budget_is_deterministic_and_enforced() -> Result<(), ExprError> {
    // The VM's unit count is pinned by the fork since norn-v2.0.1-5
    // (zen-fork-ledger.md): 3,469. Copying the 400-number result out is
    // charged on top by its recursive size, 401 (T1-EXPR-4).
    let source = "flatMap([0..19], map([0..19], # * 2))";
    let expr = Expression::compile(source)?;
    let ok = expr.evaluate(&Value::Null, EvaluationBudget::new(10_000))?;
    assert_eq!(ok.units_used(), 3469 + 401);
    let again = expr.evaluate(&Value::Null, EvaluationBudget::new(10_000))?;
    assert_eq!(again.units_used(), ok.units_used());

    assert_eq!(
        expr.evaluate(&Value::Null, EvaluationBudget::new(500)),
        Err(ExprError::BudgetExhausted {
            limit: 500,
            used: 502
        })
    );
    assert_eq!(
        expr.evaluate(&Value::Null, EvaluationBudget::new(3870))?
            .units_used(),
        3870
    );
    // The VM fits in 3,469 units; the result copy does not. Measuring the
    // copy stops at the remaining budget (0 here), so it reports one unit
    // over, however large the result.
    assert_eq!(
        expr.evaluate(&Value::Null, EvaluationBudget::new(3469)),
        Err(ExprError::BudgetExhausted {
            limit: 3469,
            used: 3470
        })
    );
    assert!(matches!(
        eval("1", &Value::Null, EvaluationBudget::new(0)),
        Err(ExprError::BudgetExhausted { limit: 0, .. })
    ));
    Ok(())
}

#[test]
fn temporal_results_project_to_canonical_strings() {
    assert_eq!(
        eval("date('2026-09-27')", &Value::Null, BUDGET).unwrap(),
        Value::from("2026-09-27")
    );
    assert_eq!(
        eval(
            "timestamp('2026-09-27T12:00:00+02:00')",
            &Value::Null,
            BUDGET
        )
        .unwrap(),
        Value::from("2026-09-27T10:00:00Z")
    );
}

#[test]
fn inexact_numbers_are_rejected() {
    for bad in [
        "1e3",
        "01",
        "79228162514264337593543950336",
        "0.00000000000000000000000000001",
    ] {
        assert!(
            matches!(bad.parse::<Number>(), Err(ExprError::InvalidNumber { .. })),
            "{bad}"
        );
    }
}

#[test]
fn empty_template_literal_is_an_empty_string() {
    // Minimized from the `zen_expression` fuzz target (#3243): a template
    // literal with zero parts made the ZEN fork's `Join` opcode compute
    // `0 - 1` separators, which panicked under overflow checks (fork S6).
    assert_eq!(eval("``", &Value::Null, BUDGET), Ok(Value::from("")));
    assert_eq!(eval("`a${1}`", &Value::Null, BUDGET), Ok(Value::from("a1")));

    // The raw fuzz input: either outcome is acceptable, a panic is not.
    let first = eval("pui-`", &context(), BUDGET);
    assert_eq!(first, eval("pui-`", &context(), BUDGET));
}

#[test]
fn nested_brackets_beside_a_range_compile_promptly() {
    // Minimized from the `zen_expression` fuzz target (#3243): with a range
    // operator in the source, the ZEN parser re-parsed every nested bracket
    // speculatively, so parse time grew exponentially (fork C11).
    let fuzzed = "()[[[[[[- n-5 not in [-5..ot in [-[[[[[[[[[[[[[[[[[[[[( 1 O5";
    let first = Expression::compile(fuzzed).err();
    assert_eq!(first, Expression::compile(fuzzed).err());

    // 3^40 parser steps before the fix; still inside the 128-bracket bound.
    let depth = 40;
    let nested = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
    let source = format!("1 in [0..2] and len({nested}) == 1");
    assert_eq!(eval(&source, &Value::Null, BUDGET), Ok(Value::Bool(true)));
}
