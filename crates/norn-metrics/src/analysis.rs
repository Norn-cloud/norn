//! Pure, bounded statistical computations over admitted temporal tables.
//!
//! Estimates are approximate binary64 computations, never exact business amounts.
//! These reports do not grant publication, causal wording, authorization or a
//! `proved` label. A host must bind registration and evidence to ledger receipts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "binary64 statistical operations are checked for finite results before returning"
)]

use norn_ir::RuntimeValue;
use serde::Serialize;
use statrs::distribution::{ContinuousCDF, StudentsT};
use thiserror::Error;

use crate::temporal::TemporalTable;

/// A statistical computation cannot produce a meaningful finite report.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AnalysisError {
    /// This first profile is one predictor with an intercept, or binary group membership.
    #[error("analysis.profile: {0}")]
    Profile(&'static str),
    /// Insufficient or degenerate data, including numerical instability.
    #[error("analysis.data: {0}")]
    Data(&'static str),
}

/// Welch's unequal-variance two-sample t-test, group zero minus group one.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WelchReport {
    /// Group counts.
    pub n: (u32, u32),
    /// Difference in group means, in outcome units.
    pub difference: f64,
    /// Welch statistic.
    pub statistic: f64,
    /// Satterthwaite degrees of freedom.
    pub degrees_of_freedom: f64,
    /// Two-sided probability.
    pub p_value: f64,
    /// Confidence interval for the mean difference.
    pub interval: (f64, f64),
    /// Conservative sample-size check; independence still needs signed attestation.
    pub sample_size_passed: bool,
}

fn count(n: usize) -> Result<f64, AnalysisError> {
    Ok(f64::from(
        u32::try_from(n).map_err(|_error| AnalysisError::Data("sample bound"))?,
    ))
}

fn number(value: &RuntimeValue) -> Result<f64, AnalysisError> {
    let RuntimeValue::Number(value) = value else {
        return Err(AnalysisError::Data("non-numeric value"));
    };
    let value: f64 = value
        .canonical()
        .parse()
        .map_err(|_error| AnalysisError::Data("binary64 conversion"))?;
    if !value.is_finite() {
        return Err(AnalysisError::Data("non-finite conversion"));
    }
    Ok(value)
}

fn profile(table: &TemporalTable, alpha: f64) -> Result<(), AnalysisError> {
    if table.plan().features().len() != 1 || !alpha.is_finite() || !(0.0 < alpha && alpha < 1.0) {
        return Err(AnalysisError::Profile(
            "one feature and alpha strictly between zero and one required",
        ));
    }
    Ok(())
}

fn student(df: f64) -> Result<StudentsT, AnalysisError> {
    StudentsT::new(0.0, 1.0, df).map_err(|_error| AnalysisError::Data("degrees of freedom"))
}

/// Compare outcome means for exact binary membership (feature 0 or 1). Units
/// come from the common ontology outcome column. No equal-variance assumption.
///
/// # Errors
/// Non-binary/dimensional membership, undersized groups or degenerate variance.
pub fn welch(table: &TemporalTable, alpha: f64) -> Result<WelchReport, AnalysisError> {
    profile(table, alpha)?;
    if table
        .plan()
        .features()
        .first()
        .is_some_and(|f| f.unit.is_some())
    {
        return Err(AnalysisError::Profile(
            "group membership must be dimensionless",
        ));
    }
    let mut groups = (Vec::new(), Vec::new());
    for row in table.rows() {
        let feature = row
            .features
            .first()
            .ok_or(AnalysisError::Data("missing feature"))?;
        let RuntimeValue::Number(group) = &feature.value else {
            return Err(AnalysisError::Data("membership must be numeric"));
        };
        let y = number(&row.outcome.value)?;
        match group.canonical().as_str() {
            "0" => groups.0.push(y),
            "1" => groups.1.push(y),
            _ => return Err(AnalysisError::Data("membership must be zero or one")),
        }
    }
    let summarize = |ys: &[f64]| -> Result<(f64, f64, f64), AnalysisError> {
        let n = count(ys.len())?;
        if n < 2.0 {
            return Err(AnalysisError::Data("two rows per group required"));
        }
        let origin = *ys.first().ok_or(AnalysisError::Data("empty group"))?;
        let centered_mean = ys.iter().map(|y| y - origin).sum::<f64>() / n;
        let mean = origin + centered_mean;
        let variance = ys
            .iter()
            .map(|y| ((y - origin) - centered_mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        Ok((n, mean, variance / n))
    };
    let (na, ma, va) = summarize(&groups.0)?;
    let (nb, mb, vb) = summarize(&groups.1)?;
    let error = (va + vb).sqrt();
    if error <= 0.0 {
        return Err(AnalysisError::Data("zero group variance"));
    }
    let df = (va + vb).powi(2) / (va.powi(2) / (na - 1.0) + vb.powi(2) / (nb - 1.0));
    let difference = ma - mb;
    let statistic = difference / error;
    let t = student(df)?;
    let p_value = 2.0 * t.sf(statistic.abs());
    let margin = t.inverse_cdf(1.0 - alpha / 2.0) * error;
    if ![difference, statistic, df, p_value, margin]
        .iter()
        .all(|x| x.is_finite())
    {
        return Err(AnalysisError::Data("non-finite computation"));
    }
    Ok(WelchReport {
        n: (
            u32::try_from(groups.0.len()).map_err(|_error| AnalysisError::Data("sample bound"))?,
            u32::try_from(groups.1.len()).map_err(|_error| AnalysisError::Data("sample bound"))?,
        ),
        difference,
        statistic,
        degrees_of_freedom: df,
        p_value,
        interval: (difference - margin, difference + margin),
        sample_size_passed: na >= 30.0 && nb >= 30.0,
    })
}
