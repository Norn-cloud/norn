//! Exact `Decimal` validation and fixed-point formatting matching `tools/norn-py/norn_py/expr.py`.

use crate::error::{CODE_EXPRESSION, PyAuthoringError, SourceSpan};

/// Maximum permitted absolute exponent for exact `Decimal` literals.
pub const MAX_DECIMAL_EXPONENT: i64 = 1_000;
/// Maximum permitted significant digit count for exact `Decimal` literals.
pub const MAX_DECIMAL_DIGITS: usize = 1_000;

/// Validates a finite, exponent- and precision-bounded decimal string and formats
/// it as fixed-point text matching Python's `format(Decimal(raw), "f")`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_EXPRESSION`] if `raw` is not a
/// valid finite decimal literal or exceeds [`MAX_DECIMAL_EXPONENT`] or
/// [`MAX_DECIMAL_DIGITS`].
pub fn format_exact_decimal(
    raw: &str,
    span: Option<SourceSpan>,
) -> Result<String, PyAuthoringError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            "empty Decimal literal is not admitted",
            span,
        ));
    }

    let (negative, rest) = if let Some(stripped) = trimmed.strip_prefix('-') {
        (true, stripped)
    } else if let Some(stripped) = trimmed.strip_prefix('+') {
        (false, stripped)
    } else {
        (false, trimmed)
    };

    if is_non_finite_token(rest) {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("non-finite Decimal '{raw}'"),
            span,
        ));
    }

    let (mantissa, exp_part) = split_exponent(rest);
    let exp_val = parse_exponent(exp_part, raw, span.as_ref())?;
    let (int_part, frac_part) = split_mantissa(mantissa, raw, span.as_ref())?;

    let frac_len = i64::try_from(frac_part.len()).ok().ok_or_else(|| {
        PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("Decimal '{raw}' exceeds admitted precision bounds"),
            span.clone(),
        )
    })?;
    let exponent = exp_val.checked_sub(frac_len).ok_or_else(|| {
        PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("Decimal '{raw}' exceeds admitted exponent bounds"),
            span.clone(),
        )
    })?;

    let sig_digits = collect_significant_digits(int_part, frac_part);
    if sig_digits.len() > MAX_DECIMAL_DIGITS {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!(
                "Decimal '{raw}' exceeds admitted precision/exponent bounds (±{MAX_DECIMAL_EXPONENT})"
            ),
            span,
        ));
    }

    let sig_len_i64 = i64::try_from(sig_digits.len()).unwrap_or(i64::MAX);
    let adjusted = exponent
        .checked_add(sig_len_i64.saturating_sub(1))
        .ok_or_else(|| {
            PyAuthoringError::with_optional_span(
                CODE_EXPRESSION,
                format!("Decimal '{raw}' exceeds admitted exponent bounds"),
                span.clone(),
            )
        })?;

    let exp_too_large = exponent
        .checked_abs()
        .is_none_or(|abs_exp| abs_exp > MAX_DECIMAL_EXPONENT);
    let adj_too_large = adjusted
        .checked_abs()
        .is_none_or(|abs_adj| abs_adj > MAX_DECIMAL_EXPONENT);
    if exp_too_large || adj_too_large {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!(
                "Decimal '{raw}' exceeds admitted precision/exponent bounds (±{MAX_DECIMAL_EXPONENT})"
            ),
            span,
        ));
    }

    Ok(render_fixed_point(negative, &sig_digits, exponent))
}

fn is_non_finite_token(token: &str) -> bool {
    token.eq_ignore_ascii_case("nan")
        || token.eq_ignore_ascii_case("snan")
        || token.eq_ignore_ascii_case("inf")
        || token.eq_ignore_ascii_case("infinity")
        || token.eq_ignore_ascii_case("qnan")
}

fn split_exponent(text: &str) -> (&str, Option<&str>) {
    if let Some((mantissa, exp)) = text.split_once('e') {
        (mantissa, Some(exp))
    } else if let Some((mantissa, exp)) = text.split_once('E') {
        (mantissa, Some(exp))
    } else {
        (text, None)
    }
}

fn parse_exponent(
    exp_part: Option<&str>,
    raw: &str,
    span: Option<&SourceSpan>,
) -> Result<i64, PyAuthoringError> {
    let Some(exp_str) = exp_part else {
        return Ok(0);
    };
    if exp_str.is_empty() {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("invalid Decimal exponent in '{raw}'"),
            span.cloned(),
        ));
    }
    let digits = exp_str
        .strip_prefix('+')
        .or_else(|| exp_str.strip_prefix('-'))
        .unwrap_or(exp_str);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("invalid Decimal exponent in '{raw}'"),
            span.cloned(),
        ));
    }
    exp_str.parse::<i64>().ok().ok_or_else(|| {
        PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!(
                "Decimal '{raw}' exceeds admitted precision/exponent bounds (±{MAX_DECIMAL_EXPONENT})"
            ),
            span.cloned(),
        )
    })
}

fn split_mantissa<'a>(
    mantissa: &'a str,
    raw: &str,
    span: Option<&SourceSpan>,
) -> Result<(&'a str, &'a str), PyAuthoringError> {
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));

    if (int_part.is_empty() && frac_part.is_empty())
        || !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("invalid Decimal literal '{raw}'"),
            span.cloned(),
        ));
    }

    Ok((int_part, frac_part))
}

fn collect_significant_digits(int_part: &str, frac_part: &str) -> String {
    let mut combined = String::with_capacity(int_part.len().saturating_add(frac_part.len()));
    combined.push_str(int_part);
    combined.push_str(frac_part);
    let trimmed = combined.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn render_fixed_point(negative: bool, sig_digits: &str, exponent: i64) -> String {
    let mut out = String::new();
    if negative {
        out.push('-');
    }

    if exponent >= 0 {
        out.push_str(sig_digits);
        let trailing_zeros = usize::try_from(exponent).unwrap_or(0);
        for _ in 0..trailing_zeros {
            out.push('0');
        }
        return out;
    }

    let frac_digits = usize::try_from(exponent.unsigned_abs()).unwrap_or(0);
    if sig_digits.len() > frac_digits {
        let split = sig_digits.len().saturating_sub(frac_digits);
        if let Some(head) = sig_digits.get(..split) {
            out.push_str(head);
        }
        out.push('.');
        if let Some(tail) = sig_digits.get(split..) {
            out.push_str(tail);
        }
    } else {
        out.push_str("0.");
        let leading_zeros = frac_digits.saturating_sub(sig_digits.len());
        for _ in 0..leading_zeros {
            out.push('0');
        }
        out.push_str(sig_digits);
    }

    out
}
