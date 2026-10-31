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
use statrs::distribution::{ChiSquared, ContinuousCDF, Normal, StudentsT};
use thiserror::Error;

use crate::temporal::{TemporalRow, TemporalTable};

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

/// A computed diagnostic; passing a diagnostic never proves its assumption.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    /// The check actually performed, rather than an interchangeable assumption name.
    pub method: &'static str,
    /// Statistic in the method's own scale.
    pub statistic: f64,
    /// Tail probability when the diagnostic has a specified null distribution.
    pub p_value: Option<f64>,
    /// Whether the declared diagnostic threshold passed.
    pub passed: bool,
}

/// Points for server-declared residual, QQ and influence plots.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticPlots {
    /// (fitted, residual), in admitted row order.
    pub residuals: Vec<(f64, f64)>,
    /// (standard normal quantile, sorted standardized residual).
    pub qq: Vec<(f64, f64)>,
    /// Cook's distance, in admitted row order.
    pub cooks_distance: Vec<f64>,
}

/// Simple OLS (one predictor plus intercept). Interpretation is associational.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OlsReport {
    /// Included observations; no rows are dropped implicitly.
    pub n: u32,
    /// Outcome-unit estimate at predictor zero.
    pub intercept: f64,
    /// Outcome units per predictor unit.
    pub slope: f64,
    /// Conventional, non-robust standard error of slope.
    pub slope_standard_error: f64,
    /// Two-sided slope test against zero.
    pub p_value: f64,
    /// Slope confidence interval at the supplied alpha.
    pub interval: (f64, f64),
    /// Computed diagnostics, each with its actual test/threshold identity.
    pub diagnostics: Vec<Diagnostic>,
    /// Plot data; the renderer makes no statistical decisions.
    pub plots: DiagnosticPlots,
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

fn pair(row: &TemporalRow) -> Result<(f64, f64), AnalysisError> {
    let feature = row
        .features
        .first()
        .ok_or(AnalysisError::Data("missing feature"))?;
    Ok((number(&feature.value)?, number(&row.outcome.value)?))
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

/// Fit a one-predictor associative model and return named diagnostics and plot points.
///
/// # Errors
/// Invalid profile, insufficient or degenerate observations, or non-finite computations.
pub fn ols(table: &TemporalTable, alpha: f64) -> Result<OlsReport, AnalysisError> {
    profile(table, alpha)?;
    let pairs = table
        .rows()
        .iter()
        .map(pair)
        .collect::<Result<Vec<_>, _>>()?;
    fit(&pairs, alpha)
}

#[allow(
    clippy::too_many_lines,
    reason = "one bounded OLS fit with its residual-derived diagnostic pack"
)]
fn fit(pairs: &[(f64, f64)], alpha: f64) -> Result<OlsReport, AnalysisError> {
    let n = count(pairs.len())?;
    if n < 4.0 {
        return Err(AnalysisError::Data("OLS needs at least four rows"));
    }
    let (origin_x, origin_y) = *pairs.first().ok_or(AnalysisError::Data("empty fit"))?;
    let centered_x = pairs.iter().map(|p| p.0 - origin_x).sum::<f64>() / n;
    let centered_y = pairs.iter().map(|p| p.1 - origin_y).sum::<f64>() / n;
    let mean_x = origin_x + centered_x;
    let mean_y = origin_y + centered_y;
    let sxx = pairs
        .iter()
        .map(|p| ((p.0 - origin_x) - centered_x).powi(2))
        .sum::<f64>();
    let sxy = pairs
        .iter()
        .map(|p| ((p.0 - origin_x) - centered_x) * ((p.1 - origin_y) - centered_y))
        .sum::<f64>();
    if !sxx.is_finite() || sxx <= 0.0 {
        return Err(AnalysisError::Data("constant or unstable predictor"));
    }
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;
    let residuals: Vec<_> = pairs
        .iter()
        .map(|p| {
            let fitted = mean_y + slope * ((p.0 - origin_x) - centered_x);
            (
                fitted,
                ((p.1 - origin_y) - centered_y) - slope * ((p.0 - origin_x) - centered_x),
            )
        })
        .collect();
    let sse = residuals.iter().map(|p| p.1.powi(2)).sum::<f64>();
    if !sse.is_finite() || sse <= 0.0 {
        return Err(AnalysisError::Data("zero or unstable residual variance"));
    }
    let variance = sse / (n - 2.0);
    let standard_error = (variance / sxx).sqrt();
    let t = student(n - 2.0)?;
    let p_value = 2.0 * t.sf((slope / standard_error).abs());
    let margin = t.inverse_cdf(1.0 - alpha / 2.0) * standard_error;
    let mut standardized: Vec<_> = residuals.iter().map(|p| p.1 / variance.sqrt()).collect();
    standardized.sort_by(f64::total_cmp);
    let normal =
        Normal::new(0.0, 1.0).map_err(|_error| AnalysisError::Data("normal distribution"))?;
    let qq = standardized
        .iter()
        .enumerate()
        .map(|(index, value)| Ok((normal.inverse_cdf((count(index)? + 0.5) / n), *value)))
        .collect::<Result<Vec<_>, AnalysisError>>()?;
    let cooks = pairs
        .iter()
        .zip(&residuals)
        .map(|(p, residual)| {
            let h = 1.0 / n + ((p.0 - origin_x) - centered_x).powi(2) / sxx;
            if h >= 1.0 {
                return Err(AnalysisError::Data("unit leverage"));
            }
            Ok(residual.1.powi(2) / (2.0 * variance) * h / (1.0 - h).powi(2))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let m2 = sse / n;
    let skew = residuals.iter().map(|p| p.1.powi(3)).sum::<f64>() / n / m2.powf(1.5);
    let kurtosis = residuals.iter().map(|p| p.1.powi(4)).sum::<f64>() / n / m2.powi(2);
    let jb = n / 6.0 * (skew.powi(2) + (kurtosis - 3.0).powi(2) / 4.0);
    let chi2 =
        ChiSquared::new(2.0).map_err(|_error| AnalysisError::Data("chi-squared distribution"))?;
    let squared_mean = sse / n;
    let squared_residual_spread = residuals
        .iter()
        .map(|p| (p.1.powi(2) - squared_mean).powi(2))
        .sum::<f64>();
    let squared_residual_cross_product = pairs
        .iter()
        .zip(&residuals)
        .map(|(p, r)| ((p.0 - origin_x) - centered_x) * (r.1.powi(2) - squared_mean))
        .sum::<f64>();
    let bp = if squared_residual_spread > 0.0 {
        n * (squared_residual_cross_product.powi(2) / sxx / squared_residual_spread).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let chi1 =
        ChiSquared::new(1.0).map_err(|_error| AnalysisError::Data("chi-squared distribution"))?;
    let dw = residuals
        .windows(2)
        .filter_map(|w| w.first().zip(w.get(1)))
        .map(|(a, b)| (b.1 - a.1).powi(2))
        .sum::<f64>()
        / sse;
    let max_cook = cooks.iter().copied().fold(0.0_f64, f64::max);
    let report = OlsReport {
        n: u32::try_from(pairs.len()).map_err(|_error| AnalysisError::Data("sample bound"))?,
        intercept,
        slope,
        slope_standard_error: standard_error,
        p_value,
        interval: (slope - margin, slope + margin),
        diagnostics: vec![
            Diagnostic {
                method: "sample_size_min_20",
                statistic: n,
                p_value: None,
                passed: n >= 20.0,
            },
            Diagnostic {
                method: "jarque_bera",
                statistic: jb,
                p_value: Some(chi2.sf(jb)),
                passed: chi2.sf(jb) >= alpha,
            },
            Diagnostic {
                method: "breusch_pagan_lm",
                statistic: bp,
                p_value: Some(chi1.sf(bp)),
                passed: chi1.sf(bp) >= alpha,
            },
            Diagnostic {
                method: "durbin_watson_1_to_3_heuristic",
                statistic: dw,
                p_value: None,
                passed: (1.0..=3.0).contains(&dw),
            },
            Diagnostic {
                method: "cooks_distance_4_over_n",
                statistic: max_cook,
                p_value: None,
                passed: max_cook <= 4.0 / n,
            },
        ],
        plots: DiagnosticPlots {
            residuals,
            qq,
            cooks_distance: cooks,
        },
    };
    if ![intercept, slope, standard_error, p_value, margin]
        .iter()
        .all(|x| x.is_finite())
        || report
            .diagnostics
            .iter()
            .any(|d| !d.statistic.is_finite() || d.p_value.is_some_and(|p| !p.is_finite()))
        || report
            .plots
            .residuals
            .iter()
            .chain(&report.plots.qq)
            .any(|p| !p.0.is_finite() || !p.1.is_finite())
        || report.plots.cooks_distance.iter().any(|v| !v.is_finite())
    {
        return Err(AnalysisError::Data("non-finite computation"));
    }
    Ok(report)
}

/// Fixed chronological holdout with approximate predictions and centered scores.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForecastReport {
    /// Source cursor claimed by the admitted table (host authentication required).
    pub source_head: u64,
    /// Strict boundary before which training labels must already be known.
    pub boundary: i64,
    /// OLS fitted only on the training partition.
    pub fit: OlsReport,
    /// Number of held-out observations.
    pub evaluation_n: u32,
    /// Held-out root mean squared error, in outcome units.
    pub rmse: f64,
    /// Training-mean baseline evaluated on the same holdout.
    pub baseline_rmse: f64,
    /// (object identity, prediction time, prediction, observed outcome).
    pub predictions: Vec<(String, i64, f64, f64)>,
}

/// Train a simple ontology-bound demand model and evaluate a future holdout.
/// Training labels are embargoed by both valid and recorded time. This method
/// does no hyperparameter search, fitting on held-out outcomes or promotion.
///
/// # Errors
/// Invalid split, unsupported numeric profile, degenerate fit or non-finite scores.
pub fn forecast(table: &TemporalTable, boundary: i64) -> Result<ForecastReport, AnalysisError> {
    profile(table, 0.05)?;
    let (train, test) = table
        .split(boundary)
        .map_err(|_error| AnalysisError::Data("invalid chronological split"))?;
    let pairs = train
        .iter()
        .map(|r| pair(r))
        .collect::<Result<Vec<_>, _>>()?;
    let fitted = fit(&pairs, 0.05)?;
    let n_train = count(pairs.len())?;
    let (origin_x, origin_y) = *pairs.first().ok_or(AnalysisError::Data("empty training set"))?;
    let centered_x = pairs.iter().map(|p| p.0 - origin_x).sum::<f64>() / n_train;
    let centered_y = pairs.iter().map(|p| p.1 - origin_y).sum::<f64>() / n_train;
    let mut predictions = Vec::with_capacity(test.len());
    let mut squared_error = 0.0;
    let mut baseline_error = 0.0;
    for row in &test {
        let (x, y) = pair(row)?;
        let centered_prediction = centered_y + fitted.slope * ((x - origin_x) - centered_x);
        let prediction = origin_y + centered_prediction;
        squared_error += ((y - origin_y) - centered_prediction).powi(2);
        baseline_error += ((y - origin_y) - centered_y).powi(2);
        predictions.push((row.key.clone(), row.at, prediction, y));
    }
    let n = count(test.len())?;
    let rmse = (squared_error / n).sqrt();
    let baseline_rmse = (baseline_error / n).sqrt();
    if !rmse.is_finite() || !baseline_rmse.is_finite() {
        return Err(AnalysisError::Data("non-finite forecast evaluation"));
    }
    Ok(ForecastReport {
        source_head: table.source_head(),
        boundary,
        fit: fitted,
        evaluation_n: u32::try_from(test.len())
            .map_err(|_error| AnalysisError::Data("sample bound"))?,
        rmse,
        baseline_rmse,
        predictions,
    })
}

