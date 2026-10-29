#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::pedantic,
    clippy::unwrap_used,
    missing_docs,
    reason = "focused contract and property tests use assertion-oriented exact arithmetic"
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

use norn_checker::quantity_ledger::lower_ledger_amount_expression;
use norn_ir::quantity_ledger::{
    LEDGER_AMOUNT_EXPRESSION_VERSION, LedgerAmountExpressionIr, LedgerAmountExpressionNode,
    LedgerAmountLoweringErrorCode, LedgerParameterIr, LinearAmountIr,
};
use proptest::prelude::*;
use serde_json::json;

fn parameter(parameter_id: &str, scale: u8) -> LedgerParameterIr {
    LedgerParameterIr {
        parameter_id: parameter_id.to_string(),
        scale,
        min_unscaled: "-1000000".to_string(),
        max_unscaled: "1000000".to_string(),
    }
}

fn literal(unscaled: &impl ToString, scale: u8) -> LedgerAmountExpressionNode {
    LedgerAmountExpressionNode::Literal {
        unscaled: unscaled.to_string(),
        scale,
    }
}

fn parameter_node(parameter_id: &str) -> LedgerAmountExpressionNode {
    LedgerAmountExpressionNode::Parameter {
        parameter_id: parameter_id.to_string(),
    }
}

fn source(result_scale: u8, expression: LedgerAmountExpressionNode) -> LedgerAmountExpressionIr {
    LedgerAmountExpressionIr {
        expression_version: LEDGER_AMOUNT_EXPRESSION_VERSION.to_string(),
        result_scale,
        expression,
    }
}

#[test]
fn exact_affine_expression_lowers_to_one_canonical_sorted_term_per_parameter() {
    // 2.50 + ((quantity * 1.25) - (quantity * 0.25)) = 2.5 + quantity.
    let expression = LedgerAmountExpressionNode::Add {
        left: Box::new(literal(&"250", 2)),
        right: Box::new(LedgerAmountExpressionNode::Subtract {
            left: Box::new(LedgerAmountExpressionNode::Multiply {
                left: Box::new(parameter_node("quantity")),
                right: Box::new(literal(&"125", 2)),
            }),
            right: Box::new(LedgerAmountExpressionNode::Multiply {
                left: Box::new(parameter_node("quantity")),
                right: Box::new(literal(&"25", 2)),
            }),
        }),
    };

    let lowered =
        lower_ledger_amount_expression(&source(2, expression), &[parameter("quantity", 2)])
            .expect("an exact affine expression must lower");

    assert_eq!(lowered.result_scale, 2);
    assert_eq!(lowered.constant_unscaled, "25");
    assert_eq!(lowered.constant_scale, 1);
    assert_eq!(lowered.terms.len(), 1);
    assert_eq!(lowered.terms[0].parameter_id, "quantity");
    assert_eq!(lowered.terms[0].coefficient_unscaled, "1");
    assert_eq!(lowered.terms[0].coefficient_scale, 0);
}

#[test]
fn nonlinear_multiplication_is_rejected_even_if_one_side_would_cancel() {
    let cancelled = LedgerAmountExpressionNode::Subtract {
        left: Box::new(parameter_node("x")),
        right: Box::new(parameter_node("x")),
    };
    let nonlinear = LedgerAmountExpressionNode::Multiply {
        left: Box::new(cancelled),
        right: Box::new(parameter_node("y")),
    };

    let failure = lower_ledger_amount_expression(
        &source(0, nonlinear),
        &[parameter("x", 0), parameter("y", 0)],
    )
    .expect_err("two parameter-dependent operands must fail closed");
    assert_eq!(
        failure.code,
        LedgerAmountLoweringErrorCode::NonlinearMultiplication
    );
    assert_eq!(failure.path, "expression");
}

#[test]
fn unknown_parameters_and_invalid_parameter_registries_are_rejected() {
    let unknown = lower_ledger_amount_expression(
        &source(0, parameter_node("missing")),
        &[parameter("declared", 0)],
    )
    .expect_err("unknown parameters must not enter LinearAmountIr");
    assert_eq!(
        unknown.code,
        LedgerAmountLoweringErrorCode::UnknownParameter
    );

    let duplicate = lower_ledger_amount_expression(
        &source(0, parameter_node("x")),
        &[parameter("x", 0), parameter("x", 0)],
    )
    .expect_err("ambiguous parameter metadata must fail closed");
    assert_eq!(
        duplicate.code,
        LedgerAmountLoweringErrorCode::DuplicateParameter
    );
}

#[test]
fn scale_loss_and_every_lowering_overflow_path_fail_closed() {
    let scale_loss = lower_ledger_amount_expression(
        &source(
            2,
            LedgerAmountExpressionNode::Multiply {
                left: Box::new(parameter_node("quantity")),
                right: Box::new(literal(&"1", 1)),
            },
        ),
        &[parameter("quantity", 2)],
    )
    .expect_err("a scale-three value cannot be represented at result scale two");
    assert_eq!(scale_loss.code, LedgerAmountLoweringErrorCode::ScaleLoss);

    let addition_overflow = lower_ledger_amount_expression(
        &source(
            0,
            LedgerAmountExpressionNode::Add {
                left: Box::new(literal(&i128::MAX, 0)),
                right: Box::new(literal(&1, 0)),
            },
        ),
        &[],
    )
    .expect_err("source arithmetic must be checked");
    assert_eq!(
        addition_overflow.code,
        LedgerAmountLoweringErrorCode::ArithmeticOverflow
    );

    let normalization_overflow =
        lower_ledger_amount_expression(&source(1, literal(&i128::MAX, 0)), &[])
            .expect_err("normalization to result scale must be checked");
    assert_eq!(
        normalization_overflow.code,
        LedgerAmountLoweringErrorCode::ArithmeticOverflow
    );
}

#[test]
fn integer_encodings_versions_and_serde_surface_are_closed() {
    let noncanonical = lower_ledger_amount_expression(&source(0, literal(&"01", 0)), &[])
        .expect_err("noncanonical integers would permit multiple semantic encodings");
    assert_eq!(
        noncanonical.code,
        LedgerAmountLoweringErrorCode::NonCanonicalInteger
    );

    let mut wrong_version = source(0, literal(&1, 0));
    wrong_version.expression_version = "future".to_string();
    let version = lower_ledger_amount_expression(&wrong_version, &[])
        .expect_err("unknown language versions must fail closed");
    assert_eq!(
        version.code,
        LedgerAmountLoweringErrorCode::UnsupportedExpressionVersion
    );

    for invalid in [
        json!({
            "expressionVersion": "1.0",
            "resultScale": 0,
            "expression": { "op": "divide", "left": {"op":"literal","unscaled":"1","scale":0}, "right": {"op":"literal","unscaled":"2","scale":0} }
        }),
        json!({
            "expressionVersion": "1.0",
            "resultScale": 0,
            "expression": { "op": "parameter", "parameterId": "x", "sql": "UPDATE ledger" }
        }),
        json!({
            "expressionVersion": "1.0",
            "resultScale": 0,
            "expression": { "op": "literal", "unscaled": "1", "scale": 0 },
            "fallback": "eval"
        }),
    ] {
        assert!(
            serde_json::from_value::<LedgerAmountExpressionIr>(invalid).is_err(),
            "unknown operations and fields must be rejected during decoding"
        );
    }
}

fn evaluate_scale_zero(amount: &LinearAmountIr, x: i128, y: i128) -> i128 {
    let mut result = amount
        .constant_unscaled
        .parse::<i128>()
        .expect("lowered constant is canonical i128");
    for term in &amount.terms {
        let coefficient = term
            .coefficient_unscaled
            .parse::<i128>()
            .expect("lowered coefficient is canonical i128");
        let parameter_value = match term.parameter_id.as_str() {
            "x" => x,
            "y" => y,
            other => panic!("unexpected generated parameter {other}"),
        };
        result += coefficient * parameter_value;
    }
    result
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn small_affine_programs_lower_to_the_same_exact_value(
        coefficient_x in -100_i16..=100,
        coefficient_y in -100_i16..=100,
        constant in -100_i16..=100,
        x in -100_i16..=100,
        y in -100_i16..=100,
    ) {
        let expression = LedgerAmountExpressionNode::Add {
            left: Box::new(LedgerAmountExpressionNode::Add {
                left: Box::new(LedgerAmountExpressionNode::Multiply {
                    left: Box::new(parameter_node("x")),
                    right: Box::new(literal(&coefficient_x, 0)),
                }),
                right: Box::new(LedgerAmountExpressionNode::Multiply {
                    left: Box::new(literal(&coefficient_y, 0)),
                    right: Box::new(parameter_node("y")),
                }),
            }),
            right: Box::new(literal(&constant, 0)),
        };
        let lowered = lower_ledger_amount_expression(
            &source(0, expression),
            &[parameter("y", 0), parameter("x", 0)],
        ).map_err(|failure| TestCaseError::fail(format!("unexpected lowering failure: {failure:?}")))?;

        let expected = i128::from(coefficient_x) * i128::from(x)
            + i128::from(coefficient_y) * i128::from(y)
            + i128::from(constant);
        prop_assert_eq!(evaluate_scale_zero(&lowered, i128::from(x), i128::from(y)), expected);
        prop_assert!(lowered.terms.windows(2).all(|pair| pair[0].parameter_id < pair[1].parameter_id));
    }
}
