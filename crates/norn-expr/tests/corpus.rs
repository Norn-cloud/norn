#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::panic,
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

//! The zen-expression corpus against the `norn-expr/4` structural grammar in
//! the `norn-zen/6` profile (T1-EXPR-3b,
//! #2521).
//!
//! `tests/fixtures/zen_corpus.json` is the fork's own evaluation corpus
//! (`standard.csv`, `date.csv`, `unary.csv`; provenance in the file). Every
//! row gets a Norn verdict, pinned in `tests/golden/zen_corpus_verdicts.txt`
//! (`NORN_BLESS=1` regenerates it; review the diff):
//!
//! - `ok`: the row is admitted, and evaluating it through `norn-expr`
//!   reproduces the corpus output;
//! - otherwise the stable codes it is rejected with. Every excluded construct
//!   in the corpus (templates, slices, dates, conversions, `%`, `^`, regex,
//!   `max`/`min`/`avg`/..., `rand`, assignments, ...) is thereby an explicit
//!   rejection fixture.
//!
//! A unary row's subject type is derived from its `$` value; a standard row
//! is checked against `json`.

mod support;

use std::fmt::Write as _;

use norn_expr::check::codes;
use norn_expr::{
    EvaluationBudget, Expression, Number, Type, UnaryTest, Value, check_expression_with,
    check_unary_with,
};
use norn_ir::ResourceLimitProfile;
use serde_json::Value as Json;

const GOLDEN: &str = "tests/golden/zen_corpus_verdicts.txt";

fn to_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Number(
            n.to_string()
                .parse::<Number>()
                .unwrap_or_else(|err| panic!("corpus number {n}: {err}")),
        ),
        Json::String(s) => Value::String(s.clone()),
        Json::Array(items) => Value::Array(items.iter().map(to_value).collect()),
        Json::Object(entries) => {
            Value::object(entries.iter().map(|(k, v)| (k.clone(), to_value(v))))
        }
    }
}

/// The declared type a column holding `value` would have.
fn subject_type(value: &Json) -> Type {
    match value {
        Json::Null => Type::Json,
        Json::Bool(_) => Type::Bool,
        Json::Number(_) => Type::Number,
        Json::String(_) => Type::text(),
        Json::Array(items) => items.first().map_or(Type::Json, subject_type).list(),
        Json::Object(_) => Type::Json,
    }
}

fn codes(diagnostics: &[norn_expr::Diagnostic]) -> String {
    let mut codes: Vec<&str> = diagnostics.iter().map(|d| d.code).collect();
    codes.sort_unstable();
    codes.dedup();
    codes.join(",")
}

fn limits() -> ResourceLimitProfile {
    ResourceLimitProfile::norn_limits_v1()
}

/// Checks one corpus row; evaluates it when admitted, under a budget of
/// exactly its static cost estimate (T1-EXPR-4). Returns the verdict.
fn verdict(env: &norn_expr::ZenTypeEnv, case: &Json) -> String {
    let source = case["expression"].as_str().expect("expression");
    let input = &case["input"];
    let output = to_value(&case["output"]);
    match case["mode"].as_str().expect("mode") {
        "standard" => {
            let expr = match Expression::compile(source) {
                Ok(expr) => expr,
                Err(_) => return codes::SYNTAX.to_owned(),
            };
            match check_expression_with(&expr, env, &Type::Json, &limits()) {
                Ok(checked) => {
                    let context = if input.is_null() {
                        Value::object::<String>([])
                    } else {
                        to_value(input)
                    };
                    // Under a budget of exactly the static estimate.
                    let value = expr
                        .evaluate(&context, EvaluationBudget::new(checked.cost().units()))
                        .unwrap_or_else(|err| panic!("admitted `{source}` failed: {err}"))
                        .into_value();
                    assert_eq!(value, output, "admitted `{source}`");
                    "ok".to_owned()
                }
                Err(diagnostics) => codes(&diagnostics),
            }
        }
        "unary" => {
            let test = match UnaryTest::compile(source) {
                Ok(test) => test,
                Err(_) => return codes::SYNTAX.to_owned(),
            };
            let subject = &input["$"];
            match check_unary_with(&test, env, &subject_type(subject), &limits()) {
                Ok(bound) => {
                    let result = test
                        .evaluate(
                            &to_value(input),
                            &to_value(subject),
                            EvaluationBudget::new(bound.units()),
                        )
                        .unwrap_or_else(|err| panic!("admitted `{source}` failed: {err}"));
                    assert_eq!(
                        Value::Bool(result),
                        output,
                        "admitted `{source}` on {subject}"
                    );
                    "ok".to_owned()
                }
                Err(diagnostics) => codes(&diagnostics),
            }
        }
        other => panic!("unknown mode {other}"),
    }
}

#[test]
fn zen_corpus_verdicts_are_pinned() {
    let corpus: Json =
        serde_json::from_str(include_str!("fixtures/zen_corpus.json")).expect("corpus decodes");
    let env = support::Model::retail().env();
    let mut actual = String::new();
    let (mut admitted, mut rejected) = (0_usize, 0_usize);
    for case in corpus["cases"].as_array().expect("cases") {
        let verdict = verdict(&env, case);
        if verdict == "ok" {
            admitted += 1;
        } else {
            rejected += 1;
        }
        writeln!(
            actual,
            "{}\t{}\t{}\t{}",
            case["file"].as_str().expect("file"),
            case["expression"].as_str().expect("expression"),
            case["input"],
            verdict
        )
        .expect("write");
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("NORN_BLESS").is_some() {
        std::fs::write(&path, &actual).expect("bless golden");
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    if actual != expected {
        let stale: Vec<&str> = actual
            .lines()
            .zip(expected.lines().chain(std::iter::repeat("")))
            .filter(|(now, pinned)| now != pinned)
            .map(|(now, _)| now)
            .collect();
        panic!(
            "{GOLDEN} is stale ({} of {} rows differ; set NORN_BLESS=1 to regenerate). Current verdicts of the differing rows:\n{}",
            stale.len(),
            actual.lines().count(),
            stale.join("\n")
        );
    }
    assert!(admitted > 200, "only {admitted} corpus rows admitted");
    assert!(rejected > 100, "only {rejected} corpus rows rejected");
}
