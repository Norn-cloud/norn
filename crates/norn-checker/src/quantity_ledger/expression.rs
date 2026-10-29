//! Closed authoring expressions for exact affine ledger amounts.
//!
//! This is a classification and lowering boundary, not a general expression
//! interpreter. Deserialization rejects unknown operations and fields. The
//! lowering pass accepts only literals, declared parameters, addition,
//! subtraction, and multiplication where at most one operand depends on a
//! parameter. Every arithmetic operation and decimal rescale is checked.

use std::collections::BTreeMap;

use norn_ir::quantity_ledger::{
    LEDGER_AMOUNT_EXPRESSION_VERSION, LedgerAmountExpressionIr, LedgerAmountExpressionNode,
    LedgerAmountLoweringError, LedgerAmountLoweringErrorCode, LedgerParameterIr, LinearAmountIr,
    LinearTermIr, MAX_LEDGER_SCALE,
};

use super::checked_pow10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScaledInteger {
    unscaled: i128,
    scale: u8,
}

impl ScaledInteger {
    const fn zero() -> Self {
        Self {
            unscaled: 0,
            scale: 0,
        }
    }

    fn canonical(mut self) -> Self {
        if self.unscaled == 0 {
            return Self::zero();
        }
        while self.scale > 0 && self.unscaled % 10 == 0 {
            self.unscaled /= 10;
            let Some(next_scale) = self.scale.checked_sub(1) else {
                break;
            };
            self.scale = next_scale;
        }
        self
    }

    fn checked_add(self, rhs: Self, path: &str) -> Result<Self, LedgerAmountLoweringError> {
        let target_scale = self.scale.max(rhs.scale);
        let lhs = checked_rescale(self, target_scale, path)?;
        let rhs = checked_rescale(rhs, target_scale, path)?;
        lhs.checked_add(rhs)
            .map(|unscaled| {
                Self {
                    unscaled,
                    scale: target_scale,
                }
                .canonical()
            })
            .ok_or_else(|| {
                error(
                    LedgerAmountLoweringErrorCode::ArithmeticOverflow,
                    path,
                    "addition overflows signed i128",
                )
            })
    }

    fn checked_sub(self, rhs: Self, path: &str) -> Result<Self, LedgerAmountLoweringError> {
        let target_scale = self.scale.max(rhs.scale);
        let lhs = checked_rescale(self, target_scale, path)?;
        let rhs = checked_rescale(rhs, target_scale, path)?;
        lhs.checked_sub(rhs)
            .map(|unscaled| {
                Self {
                    unscaled,
                    scale: target_scale,
                }
                .canonical()
            })
            .ok_or_else(|| {
                error(
                    LedgerAmountLoweringErrorCode::ArithmeticOverflow,
                    path,
                    "subtraction overflows signed i128",
                )
            })
    }

    fn checked_mul(self, rhs: Self, path: &str) -> Result<Self, LedgerAmountLoweringError> {
        let scale = self.scale.checked_add(rhs.scale).ok_or_else(|| {
            error(
                LedgerAmountLoweringErrorCode::ScaleOutOfRange,
                path,
                "multiplication scale overflows its u8 representation",
            )
        })?;
        if scale > MAX_LEDGER_SCALE {
            return Err(error(
                LedgerAmountLoweringErrorCode::ScaleOutOfRange,
                path,
                format!("multiplication scale {scale} exceeds maximum {MAX_LEDGER_SCALE}"),
            ));
        }
        self.unscaled
            .checked_mul(rhs.unscaled)
            .map(|unscaled| ScaledInteger { unscaled, scale }.canonical())
            .ok_or_else(|| {
                error(
                    LedgerAmountLoweringErrorCode::ArithmeticOverflow,
                    path,
                    "multiplication overflows signed i128",
                )
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AffineExpression {
    constant: ScaledInteger,
    coefficients: BTreeMap<String, ScaledInteger>,
    // This records syntactic/transitive dependency rather than inspecting the
    // simplified coefficient map. Therefore `(x - x) * y` is still rejected
    // as nonlinear instead of gaining authority through cancellation.
    depends_on_parameter: bool,
}

impl AffineExpression {
    fn literal(value: ScaledInteger) -> Self {
        Self {
            constant: value,
            coefficients: BTreeMap::new(),
            depends_on_parameter: false,
        }
    }

    fn parameter(parameter_id: String) -> Self {
        Self {
            constant: ScaledInteger::zero(),
            coefficients: BTreeMap::from([(
                parameter_id,
                ScaledInteger {
                    unscaled: 1,
                    scale: 0,
                },
            )]),
            depends_on_parameter: true,
        }
    }
}

/// Classify and deterministically lower a closed authoring expression.
///
/// The returned terms are sorted by parameter identifier. `resultScale` is an
/// explicit contract: if any exact source component needs more fractional
/// digits, lowering rejects rather than rounding or truncating it. This pass
/// checks its own arithmetic and result-scale normalization; parameter-bound
/// and unit-precision overflow remain the downstream pack checker's job.
pub fn lower_ledger_amount_expression(
    source: &LedgerAmountExpressionIr,
    parameters: &[LedgerParameterIr],
) -> Result<LinearAmountIr, LedgerAmountLoweringError> {
    if source.expression_version != LEDGER_AMOUNT_EXPRESSION_VERSION {
        return Err(error(
            LedgerAmountLoweringErrorCode::UnsupportedExpressionVersion,
            "expressionVersion",
            format!(
                "unsupported expression version `{}`; expected `{LEDGER_AMOUNT_EXPRESSION_VERSION}`",
                source.expression_version
            ),
        ));
    }
    if source.result_scale > MAX_LEDGER_SCALE {
        return Err(error(
            LedgerAmountLoweringErrorCode::ScaleOutOfRange,
            "resultScale",
            format!(
                "result scale {} exceeds maximum {MAX_LEDGER_SCALE}",
                source.result_scale
            ),
        ));
    }

    let parameter_scales = collect_parameter_scales(parameters)?;
    let affine = classify_expression(&source.expression, &parameter_scales, "expression")?;
    verify_result_scale(&affine, &parameter_scales, source.result_scale)?;

    Ok(LinearAmountIr {
        result_scale: source.result_scale,
        constant_unscaled: affine.constant.unscaled.to_string(),
        constant_scale: affine.constant.scale,
        terms: affine
            .coefficients
            .into_iter()
            .map(|(parameter_id, coefficient)| LinearTermIr {
                parameter_id,
                coefficient_unscaled: coefficient.unscaled.to_string(),
                coefficient_scale: coefficient.scale,
            })
            .collect(),
    })
}

fn collect_parameter_scales(
    parameters: &[LedgerParameterIr],
) -> Result<BTreeMap<&str, u8>, LedgerAmountLoweringError> {
    let mut scales = BTreeMap::new();
    for (index, parameter) in parameters.iter().enumerate() {
        let path = format!("parameters[{index}].parameterId");
        if parameter.parameter_id.is_empty()
            || parameter.parameter_id.trim() != parameter.parameter_id
        {
            return Err(error(
                LedgerAmountLoweringErrorCode::EmptyParameterId,
                path,
                "parameter identifier must be non-empty and contain no surrounding whitespace",
            ));
        }
        if parameter.scale > MAX_LEDGER_SCALE {
            return Err(error(
                LedgerAmountLoweringErrorCode::ScaleOutOfRange,
                format!("parameters[{index}].scale"),
                format!(
                    "parameter scale {} exceeds maximum {MAX_LEDGER_SCALE}",
                    parameter.scale
                ),
            ));
        }
        if scales
            .insert(parameter.parameter_id.as_str(), parameter.scale)
            .is_some()
        {
            return Err(error(
                LedgerAmountLoweringErrorCode::DuplicateParameter,
                path,
                format!("duplicate parameter `{}`", parameter.parameter_id),
            ));
        }
    }
    Ok(scales)
}

fn classify_expression(
    expression: &LedgerAmountExpressionNode,
    parameter_scales: &BTreeMap<&str, u8>,
    path: &str,
) -> Result<AffineExpression, LedgerAmountLoweringError> {
    match expression {
        LedgerAmountExpressionNode::Literal { unscaled, scale } => {
            if *scale > MAX_LEDGER_SCALE {
                return Err(error(
                    LedgerAmountLoweringErrorCode::ScaleOutOfRange,
                    format!("{path}.scale"),
                    format!("literal scale {scale} exceeds maximum {MAX_LEDGER_SCALE}"),
                ));
            }
            let value = parse_canonical_i128(unscaled, format!("{path}.unscaled"))?;
            Ok(AffineExpression::literal(
                ScaledInteger {
                    unscaled: value,
                    scale: *scale,
                }
                .canonical(),
            ))
        }
        LedgerAmountExpressionNode::Parameter { parameter_id } => {
            if !parameter_scales.contains_key(parameter_id.as_str()) {
                return Err(error(
                    LedgerAmountLoweringErrorCode::UnknownParameter,
                    format!("{path}.parameterId"),
                    format!("unknown parameter `{parameter_id}`"),
                ));
            }
            Ok(AffineExpression::parameter(parameter_id.clone()))
        }
        LedgerAmountExpressionNode::Add { left, right } => {
            let left = classify_expression(left, parameter_scales, &format!("{path}.left"))?;
            let right = classify_expression(right, parameter_scales, &format!("{path}.right"))?;
            combine_affine(left, right, false, path)
        }
        LedgerAmountExpressionNode::Subtract { left, right } => {
            let left = classify_expression(left, parameter_scales, &format!("{path}.left"))?;
            let right = classify_expression(right, parameter_scales, &format!("{path}.right"))?;
            combine_affine(left, right, true, path)
        }
        LedgerAmountExpressionNode::Multiply { left, right } => {
            let left = classify_expression(left, parameter_scales, &format!("{path}.left"))?;
            let right = classify_expression(right, parameter_scales, &format!("{path}.right"))?;
            if left.depends_on_parameter && right.depends_on_parameter {
                return Err(error(
                    LedgerAmountLoweringErrorCode::NonlinearMultiplication,
                    path,
                    "multiplication may have at most one parameter-dependent operand",
                ));
            }
            if left.depends_on_parameter {
                multiply_affine_by_constant(left, right.constant, path)
            } else if right.depends_on_parameter {
                multiply_affine_by_constant(right, left.constant, path)
            } else {
                Ok(AffineExpression::literal(
                    left.constant.checked_mul(right.constant, path)?,
                ))
            }
        }
    }
}

fn combine_affine(
    left: AffineExpression,
    right: AffineExpression,
    subtract: bool,
    path: &str,
) -> Result<AffineExpression, LedgerAmountLoweringError> {
    let constant = if subtract {
        left.constant.checked_sub(right.constant, path)?
    } else {
        left.constant.checked_add(right.constant, path)?
    };
    let mut coefficients = left.coefficients;
    for (parameter_id, rhs) in right.coefficients {
        let lhs = coefficients
            .remove(parameter_id.as_str())
            .unwrap_or_else(ScaledInteger::zero);
        let value = if subtract {
            lhs.checked_sub(rhs, path)?
        } else {
            lhs.checked_add(rhs, path)?
        };
        if value.unscaled != 0 {
            coefficients.insert(parameter_id, value);
        }
    }
    Ok(AffineExpression {
        constant,
        coefficients,
        depends_on_parameter: left.depends_on_parameter || right.depends_on_parameter,
    })
}

fn multiply_affine_by_constant(
    mut expression: AffineExpression,
    constant: ScaledInteger,
    path: &str,
) -> Result<AffineExpression, LedgerAmountLoweringError> {
    expression.constant = expression.constant.checked_mul(constant, path)?;
    let mut coefficients = BTreeMap::new();
    for (parameter_id, coefficient) in expression.coefficients {
        let value = coefficient.checked_mul(constant, path)?;
        if value.unscaled != 0 {
            coefficients.insert(parameter_id, value);
        }
    }
    expression.coefficients = coefficients;
    Ok(expression)
}

fn verify_result_scale(
    affine: &AffineExpression,
    parameter_scales: &BTreeMap<&str, u8>,
    result_scale: u8,
) -> Result<(), LedgerAmountLoweringError> {
    verify_component_scale(
        affine.constant,
        affine.constant.scale,
        result_scale,
        "expression",
    )?;
    for (parameter_id, coefficient) in &affine.coefficients {
        let parameter_scale = parameter_scales
            .get(parameter_id.as_str())
            .copied()
            .ok_or_else(|| {
                error(
                    LedgerAmountLoweringErrorCode::UnknownParameter,
                    "expression",
                    format!("unknown parameter `{parameter_id}`"),
                )
            })?;
        let source_scale = parameter_scale
            .checked_add(coefficient.scale)
            .ok_or_else(|| {
                error(
                    LedgerAmountLoweringErrorCode::ScaleOutOfRange,
                    "expression",
                    format!("parameter `{parameter_id}` and coefficient scales overflow u8"),
                )
            })?;
        verify_component_scale(
            *coefficient,
            source_scale,
            result_scale,
            &format!("expression.{parameter_id}"),
        )?;
    }
    Ok(())
}

fn verify_component_scale(
    value: ScaledInteger,
    source_scale: u8,
    result_scale: u8,
    path: &str,
) -> Result<(), LedgerAmountLoweringError> {
    let gap = result_scale.checked_sub(source_scale).ok_or_else(|| error(
        LedgerAmountLoweringErrorCode::ScaleLoss,
        path,
        format!(
            "source scale {source_scale} exceeds result scale {result_scale}; rounding is forbidden"
        ),
    ))?;
    let multiplier = checked_pow10(gap).ok_or_else(|| {
        error(
            LedgerAmountLoweringErrorCode::ArithmeticOverflow,
            path,
            "power-of-ten normalization overflows signed i128",
        )
    })?;
    value.unscaled.checked_mul(multiplier).ok_or_else(|| {
        error(
            LedgerAmountLoweringErrorCode::ArithmeticOverflow,
            path,
            "normalization to result scale overflows signed i128",
        )
    })?;
    Ok(())
}

fn checked_rescale(
    value: ScaledInteger,
    target_scale: u8,
    path: &str,
) -> Result<i128, LedgerAmountLoweringError> {
    let gap = target_scale.checked_sub(value.scale).ok_or_else(|| {
        error(
            LedgerAmountLoweringErrorCode::ScaleLoss,
            path,
            "internal rescale would discard precision",
        )
    })?;
    let multiplier = checked_pow10(gap).ok_or_else(|| {
        error(
            LedgerAmountLoweringErrorCode::ArithmeticOverflow,
            path,
            "power-of-ten alignment overflows signed i128",
        )
    })?;
    value.unscaled.checked_mul(multiplier).ok_or_else(|| {
        error(
            LedgerAmountLoweringErrorCode::ArithmeticOverflow,
            path,
            "scale alignment overflows signed i128",
        )
    })
}

fn parse_canonical_i128(value: &str, path: String) -> Result<i128, LedgerAmountLoweringError> {
    let parsed = value.parse::<i128>().map_err(|_parse_error| {
        error(
            LedgerAmountLoweringErrorCode::InvalidInteger,
            path.clone(),
            format!("`{value}` is not a signed i128 decimal integer"),
        )
    })?;
    if parsed.to_string() != value {
        return Err(error(
            LedgerAmountLoweringErrorCode::NonCanonicalInteger,
            path,
            format!("`{value}` is not the canonical decimal encoding `{parsed}`"),
        ));
    }
    Ok(parsed)
}

fn error(
    code: LedgerAmountLoweringErrorCode,
    path: impl Into<String>,
    message: impl Into<String>,
) -> LedgerAmountLoweringError {
    LedgerAmountLoweringError {
        code,
        path: path.into(),
        message: message.into(),
    }
}
